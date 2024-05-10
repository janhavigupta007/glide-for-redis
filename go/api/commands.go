// Copyright GLIDE-for-Redis Project Contributors - SPDX Identifier: Apache-2.0

package api

// StringCommands defines an interface for the "String Commands" group of Redis commands for standalone and cluster clients.
//
// See [redis.io] for details.
//
// [redis.io]: https://redis.io/commands/?group=string
type StringCommands interface {
	// Set the given key with the given value. The return value is a response from Redis containing the string "OK".
	//
	// See [redis.io] for details.
	//
	// For example:
	//
	//	result := client.Set("key", "value")
	//
	// [redis.io]: https://redis.io/commands/set/
	Set(key string, value string) (string, error)

	// SetWithOptions sets the given key with the given value using the given options. The return value is dependent on the
	// passed options. If the value is successfully set, "OK" is returned. If value isn't set because of [OnlyIfExists] or
	// [OnlyIfDoesNotExist] conditions, a zero-value string is returned (""). If [SetOptions#ReturnOldValue] is set, the old
	// value is returned.
	//
	// See [redis.io] for details.
	//
	// For example:
	//
	//  result, err := client.SetWithOptions("key", "value", &api.SetOptions{
	//      ConditionalSet: api.OnlyIfExists,
	//      Expiry: &api.Expiry{
	//          Type: api.Seconds,
	//          Count: uint64(5),
	//      },
	//  })
	//
	// [redis.io]: https://redis.io/commands/set/
	SetWithOptions(key string, value string, options *SetOptions) (string, error)

	// Get a pointer to the value associated with the given key, or nil if no such value exists
	//
	// See [redis.io] for details.
	//
	// For example:
	//
	//	result := client.Set("key", "value")
	//
	// [redis.io]: https://redis.io/commands/set/
	Get(key string) (string, error)

	MSet(keyValueMap map[string]string) (string, error)

	MGet(keys []string) ([]string, error)
}

type GenericBaseCommands interface {
	Del(keys []string) (int64, error)

	Exists(keys []string) (int64, error)
}
