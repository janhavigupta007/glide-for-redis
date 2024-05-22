/*
 * Copyright GLIDE-for-Redis Project Contributors - SPDX Identifier: Apache-2.0
 */

// TODO: Investigate using uniffi bindings for Go instead of cbindgen
// TODO: Uncomment this line after Rust code is finalized. It is commented out to avoid compilation errors about unsafe usage.
// #![deny(unsafe_op_in_unsafe_fn)]
use glide_core::client::Client as GlideClient;
use glide_core::connection_request;
use glide_core::errors;
use glide_core::errors::RequestErrorType;
use glide_core::ConnectionRequest;
use protobuf::Message;
use redis::{cmd, Cmd, FromRedisValue, RedisResult, Value};
use std::{
    ffi::{c_void, CStr, CString},
    mem,
    os::raw::{c_char, c_long, c_double},
};
use tokio::runtime::Builder;
use tokio::runtime::Runtime;

#[repr(C)]
pub struct CommandResponse {
    int_value: c_long,
    float_value: c_double,
    string_value: *mut c_char,
    int_array_value: *mut c_long,
    array_value: *mut *mut c_char,
}

/// Success callback that is called when a Redis command succeeds.
///
/// The success callback needs to copy the given string synchronously, since it will be dropped by Rust once the callback returns. The callback should be offloaded to a separate thread in order not to exhaust the client's thread pool.
///
/// `index_ptr` is a baton-pass back to the caller language to uniquely identify the promise.
/// `message` is the value returned by the Redis command. The 'message' is managed by Rust and is freed when the callback returns control back to the caller.
// TODO: Change message type when implementing command logic
// TODO: Consider using a single response callback instead of success and failure callbacks
pub type SuccessCallback =
    unsafe extern "C" fn(index_ptr: usize, message: *const CommandResponse) -> ();

/// Failure callback that is called when a Redis command fails.
///
/// The failure callback needs to copy the given string synchronously, since it will be dropped by Rust once the callback returns. The callback should be offloaded to a separate thread in order not to exhaust the client's thread pool.
///
/// `index_ptr` is a baton-pass back to the caller language to uniquely identify the promise.
/// `error_message` is the error message returned by Redis for the failed command. The 'error_message' is managed by Rust and is freed when the callback returns control back to the caller.
/// `error_type` is the type of error returned by glide-core, depending on the `RedisError` returned.
pub type FailureCallback = unsafe extern "C" fn(
    index_ptr: usize,
    error_message: *const c_char,
    error_type: RequestErrorType,
) -> ();

/// The connection response.
///
/// It contains either a connection or an error. It is represented as a struct instead of a union for ease of use in the wrapper language.
///
/// The struct is freed by the external caller by using `free_connection_response` to avoid memory leaks.
#[repr(C)]
pub struct ConnectionResponse {
    conn_ptr: *const c_void,
    connection_error_message: *const c_char,
}

/// A `GlideClient` adapter.
// TODO: Remove allow(dead_code) once connection logic is implemented
#[allow(dead_code)]
pub struct ClientAdapter {
    client: GlideClient,
    success_callback: SuccessCallback,
    failure_callback: FailureCallback,
    runtime: Runtime,
}

fn create_client_internal(
    connection_request_bytes: &[u8],
    success_callback: SuccessCallback,
    failure_callback: FailureCallback,
) -> Result<ClientAdapter, String> {
    let request = connection_request::ConnectionRequest::parse_from_bytes(connection_request_bytes)
        .map_err(|err| err.to_string())?;
    // TODO: optimize this using multiple threads instead of a single worker thread (e.g. by pinning each go thread to a rust thread)
    let runtime = Builder::new_multi_thread()
        .enable_all()
        .worker_threads(1)
        .thread_name("GLIDE for Redis Go thread")
        .build()
        .map_err(|err| {
            let redis_error = err.into();
            errors::error_message(&redis_error)
        })?;
    let client = runtime
        .block_on(GlideClient::new(ConnectionRequest::from(request)))
        .map_err(|err| err.to_string())?;
    Ok(ClientAdapter {
        client,
        success_callback,
        failure_callback,
        runtime,
    })
}

/// Creates a new `ClientAdapter` with a new `GlideClient` configured using a Protobuf `ConnectionRequest`.
///
/// The returned `ConnectionResponse` will only be freed by calling [`free_connection_response`].
///
/// `connection_request_bytes` is an array of bytes that will be parsed into a Protobuf `ConnectionRequest` object.
/// `connection_request_len` is the number of bytes in `connection_request_bytes`.
/// `success_callback` is the callback that will be called when a Redis command succeeds.
/// `failure_callback` is the callback that will be called when a Redis command fails.
///
/// # Safety
///
/// * `connection_request_bytes` must point to `connection_request_len` consecutive properly initialized bytes. It must be a well-formed Protobuf `ConnectionRequest` object. The array must be allocated by the caller and subsequently freed by the caller after this function returns.
/// * `connection_request_len` must not be greater than the length of the connection request bytes array. It must also not be greater than the max value of a signed pointer-sized integer.
/// * The `conn_ptr` pointer in the returned `ConnectionResponse` must live while the client is open/active and must be explicitly freed by calling [`close_client`].
/// * The `connection_error_message` pointer in the returned `ConnectionResponse` must live until the returned `ConnectionResponse` pointer is passed to [`free_connection_response`].
/// * Both the `success_callback` and `failure_callback` function pointers need to live while the client is open/active. The caller is responsible for freeing both callbacks.
// TODO: Consider making this async
#[no_mangle]
pub unsafe extern "C" fn create_client(
    connection_request_bytes: *const u8,
    connection_request_len: usize,
    success_callback: SuccessCallback,
    failure_callback: FailureCallback,
) -> *const ConnectionResponse {
    let request_bytes =
        unsafe { std::slice::from_raw_parts(connection_request_bytes, connection_request_len) };
    let response = match create_client_internal(request_bytes, success_callback, failure_callback) {
        Err(err) => ConnectionResponse {
            conn_ptr: std::ptr::null(),
            connection_error_message: CString::into_raw(CString::new(err).unwrap()),
        },
        Ok(client) => ConnectionResponse {
            conn_ptr: Box::into_raw(Box::new(client)) as *const c_void,
            connection_error_message: std::ptr::null(),
        },
    };
    Box::into_raw(Box::new(response))
}

/// Closes the given `GlideClient`, freeing it from the heap.
///
/// `client_adapter_ptr` is a pointer to a valid `GlideClient` returned in the `ConnectionResponse` from [`create_client`].
///
/// # Panics
///
/// This function panics when called with a null `client_adapter_ptr`.
///
/// # Safety
///
/// * `close_client` can only be called once per client. Calling it twice is undefined behavior, since the address will be freed twice.
/// * `close_client` must be called after `free_connection_response` has been called to avoid creating a dangling pointer in the `ConnectionResponse`.
/// * `client_adapter_ptr` must be obtained from the `ConnectionResponse` returned from [`create_client`].
/// * `client_adapter_ptr` must be valid until `close_client` is called.
// TODO: Ensure safety when command has not completed yet
#[no_mangle]
pub unsafe extern "C" fn close_client(client_adapter_ptr: *const c_void) {
    assert!(!client_adapter_ptr.is_null());
    drop(unsafe { Box::from_raw(client_adapter_ptr as *mut ClientAdapter) });
}

/// Deallocates a `ConnectionResponse`.
///
/// This function also frees the contained error. If the contained error is a null pointer, the function returns and only the `ConnectionResponse` is freed.
///
/// # Panics
///
/// This function panics when called with a null `ConnectionResponse` pointer.
///
/// # Safety
///
/// * `free_connection_response` can only be called once per `ConnectionResponse`. Calling it twice is undefined behavior, since the address will be freed twice.
/// * `connection_response_ptr` must be obtained from the `ConnectionResponse` returned from [`create_client`].
/// * `connection_response_ptr` must be valid until `free_connection_response` is called.
/// * The contained `connection_error_message` must be obtained from the `ConnectionResponse` returned from [`create_client`].
/// * The contained `connection_error_message` must be valid until `free_connection_response` is called and it must outlive the `ConnectionResponse` that contains it.
#[no_mangle]
pub unsafe extern "C" fn free_connection_response(
    connection_response_ptr: *mut ConnectionResponse,
) {
    assert!(!connection_response_ptr.is_null());
    let connection_response = unsafe { Box::from_raw(connection_response_ptr) };
    let connection_error_message = connection_response.connection_error_message;
    drop(connection_response);
    if !connection_error_message.is_null() {
        drop(unsafe { CString::from_raw(connection_error_message as *mut c_char) });
    }
}

/// Deallocates a `CommandResponse`.
///
/// This function also frees the contained string_value and array_value. If the string_value and array_value are null pointers, the function returns and only the `CommandResponse` is freed.
///
/// # Panics
///
/// This function panics when called with a null `CommandResponse` pointer.
///
/// # Safety
///
/// * `free_command_response` can only be called once per `CommandResponse`. Calling it twice is undefined behavior, since the address will be freed twice.
/// * `command_response_ptr` must be obtained from the `CommandResponse` returned in [`success_callback`] from [`command`].
/// * `command_response_ptr` must be valid until `free_command_response` is called.
/// * The contained `string_value` must be obtained from the `CommandResponse` returned in [`success_callback`] from [`command`].
/// * The contained `string_value` must be valid until `free_command_response` is called and it must outlive the `CommandResponse` that contains it.
/// * The contained `int_array_value` must be obtained from the `CommandResponse` returned in [`success_callback`] from [`command`].
/// * The contained `int_array_value` must be valid until `free_command_response` is called and it must outlive the `CommandResponse` that contains it.
/// * The contained `array_value` must be obtained from the `CommandResponse` returned in [`success_callback`] from [`command`].
/// * The contained `array_value` must be valid until `free_command_response` is called and it must outlive the `CommandResponse` that contains it.
#[no_mangle]
pub unsafe extern "C" fn free_command_response(command_response_ptr: *mut CommandResponse) {
    assert!(!command_response_ptr.is_null());
    let command_response = unsafe { Box::from_raw(command_response_ptr) };
    let int_value = command_response.int_value;
    let string_value = command_response.string_value;
    let int_array_value = command_response.int_array_value;
    let array_value = command_response.array_value;
    drop(command_response);
    if !string_value.is_null() {
        let len = int_value as usize;
        Vec::from_raw_parts(string_value, len, len);
    }
    if !int_array_value.is_null() {
        let len = int_value as usize;
        Vec::from_raw_parts(int_array_value, len, len);
    }
    if !array_value.is_null() {
        let len = int_value as usize;
        let v = Vec::from_raw_parts(array_value, len, len);
        for elem in v {
            if !elem.is_null() {
                let s = CString::from_raw(elem);
                mem::drop(s);
            }
        }
    }
}

// TODO: The Rust code below is not polished - some work is required to get it up to the desired standards to be merged into main.
// The code below simply exists to get commands working for now.

// Cannot use glide_core::redis_request::RequestType, because it is not FFI safe
#[repr(u32)]
pub enum RequestType {
    // copied from redis_request.proto
    CustomCommand = 1,
    GetString = 2,
    SetString = 3,
    Ping = 4,
    Info = 5,
    Del = 6,
    Select = 7,
    ConfigGet = 8,
    ConfigSet = 9,
    ConfigResetStat = 10,
    ConfigRewrite = 11,
    ClientGetName = 12,
    ClientGetRedir = 13,
    ClientId = 14,
    ClientInfo = 15,
    ClientKill = 16,
    ClientList = 17,
    ClientNoEvict = 18,
    ClientNoTouch = 19,
    ClientPause = 20,
    ClientReply = 21,
    ClientSetInfo = 22,
    ClientSetName = 23,
    ClientUnblock = 24,
    ClientUnpause = 25,
    Expire = 26,
    HashSet = 27,
    HashGet = 28,
    HashDel = 29,
    HashExists = 30,
    MGet = 31,
    MSet = 32,
    Incr = 33,
    IncrBy = 34,
    Decr = 35,
    IncrByFloat = 36,
    DecrBy = 37,
    HashGetAll = 38,
    HashMSet = 39,
    HashMGet = 40,
    HashIncrBy = 41,
    HashIncrByFloat = 42,
    LPush = 43,
    LPop = 44,
    RPush = 45,
    RPop = 46,
    LLen = 47,
    LRem = 48,
    LRange = 49,
    LTrim = 50,
    SAdd = 51,
    SRem = 52,
    SMembers = 53,
    SCard = 54,
    PExpireAt = 55,
    PExpire = 56,
    ExpireAt = 57,
    Exists = 58,
    Unlink = 59,
    TTL = 60,
    Zadd = 61,
    Zrem = 62,
    Zrange = 63,
    Zcard = 64,
    Zcount = 65,
    ZIncrBy = 66,
    ZScore = 67,
    Type = 68,
    HLen = 69,
    Echo = 70,
    ZPopMin = 71,
    Strlen = 72,
    Lindex = 73,
    ZPopMax = 74,
    XRead = 75,
    XAdd = 76,
    XReadGroup = 77,
    XAck = 78,
    XTrim = 79,
    XGroupCreate = 80,
    XGroupDestroy = 81,
    HSetNX = 82,
    SIsMember = 83,
    HVals = 84,
    PTTL = 85,
    ZRemRangeByRank = 86,
    Persist = 87,
    ZRemRangeByScore = 88,
    Time = 89,
    ZRank = 90,
    Rename = 91,
    DBSize = 92,
    BRPop = 93,
    HKeys = 94,
    SPop = 95,
    PfAdd = 96,
    PfCount = 97,
    PfMerge = 98,
    BLPop = 100,
    LInsert = 101,
    RPushX = 102,
    LPushX = 103,
    ZMScore = 104,
    ZDiff = 105,
    ZDiffStore = 106,
    SetRange = 107,
    ZRemRangeByLex = 108,
    ZLexCount = 109,
    Append = 110,
    SUnionStore = 111,
    SDiffStore = 112,
    SInter = 113,
    SInterStore = 114,
    ZRangeStore = 115,
    GetRange = 116,
}

// copied from glide_core::socket_listener::get_command
fn get_command(request_type: RequestType) -> Option<Cmd> {
    match request_type {
        //RequestType::InvalidRequest => None,
        RequestType::CustomCommand => Some(Cmd::new()),
        RequestType::GetString => Some(cmd("GET")),
        RequestType::SetString => Some(cmd("SET")),
        RequestType::Ping => Some(cmd("PING")),
        RequestType::Info => Some(cmd("INFO")),
        RequestType::Del => Some(cmd("DEL")),
        RequestType::Select => Some(cmd("SELECT")),
        RequestType::ConfigGet => Some(get_two_word_command("CONFIG", "GET")),
        RequestType::ConfigSet => Some(get_two_word_command("CONFIG", "SET")),
        RequestType::ConfigResetStat => Some(get_two_word_command("CONFIG", "RESETSTAT")),
        RequestType::ConfigRewrite => Some(get_two_word_command("CONFIG", "REWRITE")),
        RequestType::ClientGetName => Some(get_two_word_command("CLIENT", "GETNAME")),
        RequestType::ClientGetRedir => Some(get_two_word_command("CLIENT", "GETREDIR")),
        RequestType::ClientId => Some(get_two_word_command("CLIENT", "ID")),
        RequestType::ClientInfo => Some(get_two_word_command("CLIENT", "INFO")),
        RequestType::ClientKill => Some(get_two_word_command("CLIENT", "KILL")),
        RequestType::ClientList => Some(get_two_word_command("CLIENT", "LIST")),
        RequestType::ClientNoEvict => Some(get_two_word_command("CLIENT", "NO-EVICT")),
        RequestType::ClientNoTouch => Some(get_two_word_command("CLIENT", "NO-TOUCH")),
        RequestType::ClientPause => Some(get_two_word_command("CLIENT", "PAUSE")),
        RequestType::ClientReply => Some(get_two_word_command("CLIENT", "REPLY")),
        RequestType::ClientSetInfo => Some(get_two_word_command("CLIENT", "SETINFO")),
        RequestType::ClientSetName => Some(get_two_word_command("CLIENT", "SETNAME")),
        RequestType::ClientUnblock => Some(get_two_word_command("CLIENT", "UNBLOCK")),
        RequestType::ClientUnpause => Some(get_two_word_command("CLIENT", "UNPAUSE")),
        RequestType::Expire => Some(cmd("EXPIRE")),
        RequestType::HashSet => Some(cmd("HSET")),
        RequestType::HashGet => Some(cmd("HGET")),
        RequestType::HashDel => Some(cmd("HDEL")),
        RequestType::HashExists => Some(cmd("HEXISTS")),
        RequestType::MSet => Some(cmd("MSET")),
        RequestType::MGet => Some(cmd("MGET")),
        RequestType::Incr => Some(cmd("INCR")),
        RequestType::IncrBy => Some(cmd("INCRBY")),
        RequestType::IncrByFloat => Some(cmd("INCRBYFLOAT")),
        RequestType::Decr => Some(cmd("DECR")),
        RequestType::DecrBy => Some(cmd("DECRBY")),
        RequestType::HashGetAll => Some(cmd("HGETALL")),
        RequestType::HashMSet => Some(cmd("HMSET")),
        RequestType::HashMGet => Some(cmd("HMGET")),
        RequestType::HashIncrBy => Some(cmd("HINCRBY")),
        RequestType::HashIncrByFloat => Some(cmd("HINCRBYFLOAT")),
        RequestType::LPush => Some(cmd("LPUSH")),
        RequestType::LPop => Some(cmd("LPOP")),
        RequestType::RPush => Some(cmd("RPUSH")),
        RequestType::RPop => Some(cmd("RPOP")),
        RequestType::LLen => Some(cmd("LLEN")),
        RequestType::LRem => Some(cmd("LREM")),
        RequestType::LRange => Some(cmd("LRANGE")),
        RequestType::LTrim => Some(cmd("LTRIM")),
        RequestType::SAdd => Some(cmd("SADD")),
        RequestType::SRem => Some(cmd("SREM")),
        RequestType::SMembers => Some(cmd("SMEMBERS")),
        RequestType::SCard => Some(cmd("SCARD")),
        RequestType::PExpireAt => Some(cmd("PEXPIREAT")),
        RequestType::PExpire => Some(cmd("PEXPIRE")),
        RequestType::ExpireAt => Some(cmd("EXPIREAT")),
        RequestType::Exists => Some(cmd("EXISTS")),
        RequestType::Unlink => Some(cmd("UNLINK")),
        RequestType::TTL => Some(cmd("TTL")),
        RequestType::Zadd => Some(cmd("ZADD")),
        RequestType::Zrem => Some(cmd("ZREM")),
        RequestType::Zrange => Some(cmd("ZRANGE")),
        RequestType::Zcard => Some(cmd("ZCARD")),
        RequestType::Zcount => Some(cmd("ZCOUNT")),
        RequestType::ZIncrBy => Some(cmd("ZINCRBY")),
        RequestType::ZScore => Some(cmd("ZSCORE")),
        RequestType::Type => Some(cmd("TYPE")),
        RequestType::HLen => Some(cmd("HLEN")),
        RequestType::Echo => Some(cmd("ECHO")),
        RequestType::ZPopMin => Some(cmd("ZPOPMIN")),
        RequestType::Strlen => Some(cmd("STRLEN")),
        RequestType::Lindex => Some(cmd("LINDEX")),
        RequestType::ZPopMax => Some(cmd("ZPOPMAX")),
        RequestType::XAck => Some(cmd("XACK")),
        RequestType::XAdd => Some(cmd("XADD")),
        RequestType::XReadGroup => Some(cmd("XREADGROUP")),
        RequestType::XRead => Some(cmd("XREAD")),
        RequestType::XGroupCreate => Some(get_two_word_command("XGROUP", "CREATE")),
        RequestType::XGroupDestroy => Some(get_two_word_command("XGROUP", "DESTROY")),
        RequestType::XTrim => Some(cmd("XTRIM")),
        RequestType::HSetNX => Some(cmd("HSETNX")),
        RequestType::SIsMember => Some(cmd("SISMEMBER")),
        RequestType::HVals => Some(cmd("HVALS")),
        RequestType::PTTL => Some(cmd("PTTL")),
        RequestType::ZRemRangeByRank => Some(cmd("ZREMRANGEBYRANK")),
        RequestType::Persist => Some(cmd("PERSIST")),
        RequestType::ZRemRangeByScore => Some(cmd("ZREMRANGEBYSCORE")),
        RequestType::Time => Some(cmd("TIME")),
        RequestType::ZRank => Some(cmd("ZRANK")),
        RequestType::Rename => Some(cmd("RENAME")),
        RequestType::DBSize => Some(cmd("DBSIZE")),
        RequestType::BRPop => Some(cmd("BRPOP")),
        RequestType::HKeys => Some(cmd("HKEYS")),
        RequestType::SPop => Some(cmd("SPOP")),
        RequestType::PfAdd => Some(cmd("PFADD")),
        RequestType::PfCount => Some(cmd("PFCOUNT")),
        RequestType::PfMerge => Some(cmd("PFMERGE")),
        RequestType::RPushX => Some(cmd("RPUSHX")),
        RequestType::LPushX => Some(cmd("LPUSHX")),
        RequestType::BLPop => Some(cmd("BLPOP")),
        RequestType::LInsert => Some(cmd("LINSERT")),
        RequestType::ZMScore => Some(cmd("ZMSCORE")),
        RequestType::ZDiff => Some(cmd("ZDIFF")),
        RequestType::ZDiffStore => Some(cmd("ZDIFFSTORE")),
        RequestType::SetRange => Some(cmd("SETRANGE")),
        RequestType::ZRemRangeByLex => Some(cmd("ZREMRANGEBYLEX")),
        RequestType::ZLexCount => Some(cmd("ZLEXCOUNT")),
        RequestType::Append => Some(cmd("APPEND")),
        RequestType::SUnionStore => Some(cmd("SUNIONSTORE")),
        RequestType::SDiffStore => Some(cmd("SDIFFSTORE")),
        RequestType::SInter => Some(cmd("SINTER")),
        RequestType::SInterStore => Some(cmd("SINTERSTORE")),
        RequestType::ZRangeStore => Some(cmd("ZRANGESTORE")),
        RequestType::GetRange => Some(cmd("GETRANGE")),
    }
}

// copied from glide_core::socket_listener::get_two_word_command
fn get_two_word_command(first: &str, second: &str) -> Cmd {
    let mut cmd = cmd(first);
    cmd.arg(second);
    cmd
}

use std::slice::from_raw_parts;
use std::str::Utf8Error;

// TODO: Finish documentation
/// Converts a double pointer to a vec.
///
/// # Safety
///
/// * TODO: finish safety section.
pub unsafe fn convert_double_pointer_to_vec(
    data: *const *const c_char,
    len: usize,
) -> Result<Vec<String>, Utf8Error> {
    from_raw_parts(data, len)
        .iter()
        .map(|arg| CStr::from_ptr(*arg).to_str().map(ToString::to_string))
        .collect()
}

// TODO: Finish documentation
/// Executes a command.
///
/// # Safety
///
/// * TODO: finish safety section.
#[no_mangle]
pub unsafe extern "C" fn command(
    client_adapter_ptr: *const c_void,
    channel: usize,
    command_type: RequestType,
    arg_count: usize,
    args: *const *const c_char,
) {
    let client_adapter =
        unsafe { Box::leak(Box::from_raw(client_adapter_ptr as *mut ClientAdapter)) };
    // The safety of this needs to be ensured by the calling code. Cannot dispose of the pointer before all operations have completed.
    let ptr_address = client_adapter_ptr as usize;

    let arg_vec = unsafe { convert_double_pointer_to_vec(args, arg_count) }.unwrap(); // TODO check

    let mut client_clone = client_adapter.client.clone();
    client_adapter.runtime.spawn(async move {
        let mut cmd = get_command(command_type).unwrap(); // TODO check cmd
                                                          //print!("{:?}", cmd.args);
        cmd.arg(arg_vec);

        let result = client_clone.send_command(&cmd, None).await;
        let client_adapter = unsafe { Box::leak(Box::from_raw(ptr_address as *mut ClientAdapter)) };
        let value = match result {
            Ok(value) => value,
            Err(err) => {
                println!(" === err {:?}", err);
                let message = errors::error_message(&err);
                let error_type = errors::error_type(&err);

                let c_err_str = CString::into_raw(CString::new(message).unwrap());
                unsafe { (client_adapter.failure_callback)(channel, c_err_str, error_type) };
                return;
            }
        };

        //print!(" === val {:?}\n", value.clone());

        let result: RedisResult<Option<CommandResponse>> = match value {
            Value::Nil => Ok(None),
            Value::Int(num) => Ok(Some(CommandResponse {
                int_value: num,
                float_value: 0.0,
                string_value: std::ptr::null_mut(),
                int_array_value: std::ptr::null_mut(),
                array_value: std::ptr::null_mut(),
            })),
            Value::SimpleString(text) => {
                let mut arr = text.chars().map(|b| b as c_char).collect::<Vec<_>>();
                arr.shrink_to_fit();
                let val = arr.as_mut_ptr();
                let len = arr.len() as c_long;
                std::mem::forget(arr);
                Ok(Some(CommandResponse {
                    int_value: len,
                    float_value: 0.0,
                    string_value: val,
                    int_array_value: std::ptr::null_mut(),
                    array_value: std::ptr::null_mut(),
                }))
            }
            Value::BulkString(text) => {
                let mut arr = text.iter().map(|b| *b as c_char).collect::<Vec<_>>();
                arr.shrink_to_fit();
                let val = arr.as_mut_ptr();
                let len = arr.len() as c_long;
                std::mem::forget(arr);
                Ok(Some(CommandResponse {
                    int_value: len,
                    float_value: 0.0,
                    string_value: val,
                    int_array_value: std::ptr::null_mut(),
                    array_value: std::ptr::null_mut(),
                }))
            }
            Value::VerbatimString { format: _, text } => {
                let mut arr = text.chars().map(|b| b as c_char).collect::<Vec<_>>();
                arr.shrink_to_fit();
                let val = arr.as_mut_ptr();
                let len = arr.len() as c_long;
                std::mem::forget(arr);
                Ok(Some(CommandResponse {
                    int_value: len,
                    float_value: 0.0,
                    string_value: val,
                    int_array_value: std::ptr::null_mut(),
                    array_value: std::ptr::null_mut(),
                }))
            }
            Value::Okay => {
                let mut arr = "OK".chars().map(|b| b as c_char).collect::<Vec<_>>();
                arr.shrink_to_fit();
                let val = arr.as_mut_ptr();
                let len = arr.len() as c_long;
                std::mem::forget(arr);
                Ok(Some(CommandResponse {
                    int_value: len,
                    float_value: 0.0,
                    string_value: val,
                    int_array_value: std::ptr::null_mut(),
                    array_value: std::ptr::null_mut(),
                }))
            }
            Value::Double(num) => {
                Ok(Some(CommandResponse {
                    int_value: 0,
                    float_value: num,
                    string_value: std::ptr::null_mut(),
                    int_array_value: std::ptr::null_mut(),
                    array_value: std::ptr::null_mut(),
                }))
            }
            Value::Boolean(bool) => {
                let val = CString::into_raw(CString::new(format!("{}", bool)).unwrap());
                Ok(Some(CommandResponse {
                    int_value: 0,
                    float_value: 0.0,
                    string_value: val,
                    int_array_value: std::ptr::null_mut(),
                    array_value: std::ptr::null_mut(),
                }))
            }
            Value::Array(array) => {
                let mut vlen: Vec<c_long> = Vec::new();
                let mut arr = array
                    .iter()
                    .map(|x| {
                        if *x == Value::Nil {
                            vlen.push(0);
                            return std::ptr::null_mut();
                        } else {
                            let mut res = <String>::from_owned_redis_value(x.clone()).unwrap().chars().map(|b| b as c_char).collect::<Vec<_>>();
                            res.shrink_to_fit();
                            let res_ptr = res.as_mut_ptr();
                            vlen.push(res.len() as c_long);
                            std::mem::forget(res);
                            return res_ptr;   
                        }
                    })
                    .collect::<Vec<_>>();
                arr.shrink_to_fit();
                let val = arr.as_mut_ptr();
                let len = arr.len() as c_long;
                std::mem::forget(arr);
                vlen.shrink_to_fit();
                let int_val = vlen.as_mut_ptr();
                std::mem::forget(vlen);
                Ok(Some(CommandResponse {
                    int_value: len,
                    float_value: 0.0,
                    string_value: std::ptr::null_mut(),
                    int_array_value: int_val,
                    array_value: val,
                }))
            }
            _ => todo!(),
        };

        //print!(" === result {:?}\n", result);

        unsafe {
            match result {
                Ok(None) => (client_adapter.success_callback)(channel, std::ptr::null()),
                Ok(Some(message)) => {
                    (client_adapter.success_callback)(channel, Box::into_raw(Box::new(message)))
                }
                Err(err) => {
                    let message = errors::error_message(&err);
                    let error_type = errors::error_type(&err);

                    let c_err_str = CString::into_raw(CString::new(message).unwrap());
                    (client_adapter.failure_callback)(channel, c_err_str, error_type);
                }
            };
        }
    });
}
