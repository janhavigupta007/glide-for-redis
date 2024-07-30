/**
 * Copyright Valkey GLIDE Project Contributors - SPDX Identifier: Apache-2.0
 */

import {
    afterAll,
    afterEach,
    beforeAll,
    describe,
    expect,
    it,
} from "@jest/globals";
import { BufferReader, BufferWriter } from "protobufjs";
import { v4 as uuidv4 } from "uuid";
import { GlideClient, ProtocolVersion, Transaction } from "..";
import { RedisCluster } from "../../utils/TestUtils.js";
import { FlushMode } from "../build-ts/src/Commands";
import { command_request } from "../src/ProtobufMessage";
import { runBaseTests } from "./SharedTests";
import {
    checkFunctionListResponse,
    checkSimple,
    convertStringArrayToBuffer,
    flushAndCloseClient,
    generateLuaLibCode,
    getClientConfigurationOption,
    intoString,
    parseCommandLineArgs,
    parseEndpoints,
    transactionTest,
    validateTransactionResponse,
} from "./TestUtilities";
import { ListDirection } from "..";

/* eslint-disable @typescript-eslint/no-var-requires */

type Context = {
    client: GlideClient;
};

const TIMEOUT = 50000;

describe("GlideClient", () => {
    let testsFailed = 0;
    let cluster: RedisCluster;
    let client: GlideClient;
    beforeAll(async () => {
        const standaloneAddresses =
            parseCommandLineArgs()["standalone-endpoints"];
        // Connect to cluster or create a new one based on the parsed addresses
        cluster = standaloneAddresses
            ? RedisCluster.initFromExistingCluster(
                  parseEndpoints(standaloneAddresses),
              )
            : await RedisCluster.createCluster(false, 1, 1);
    }, 20000);

    afterEach(async () => {
        await flushAndCloseClient(false, cluster.getAddresses(), client);
    });

    afterAll(async () => {
        if (testsFailed === 0) {
            await cluster.close();
        }
    }, TIMEOUT);

    it("test protobuf encode/decode delimited", () => {
        // This test is required in order to verify that the autogenerated protobuf
        // files has been corrected and the encoding/decoding works as expected.
        // See "Manually compile protobuf files" in node/README.md to get more info about the fix.
        const writer = new BufferWriter();
        const request = {
            callbackIdx: 1,
            singleCommand: {
                requestType: 2,
                argsArray: command_request.Command.ArgsArray.create({
                    args: convertStringArrayToBuffer(["bar1", "bar2"]),
                }),
            },
        };
        const request2 = {
            callbackIdx: 3,
            singleCommand: {
                requestType: 4,
                argsArray: command_request.Command.ArgsArray.create({
                    args: convertStringArrayToBuffer(["bar3", "bar4"]),
                }),
            },
        };
        command_request.CommandRequest.encodeDelimited(request, writer);
        command_request.CommandRequest.encodeDelimited(request2, writer);
        const buffer = writer.finish();
        const reader = new BufferReader(buffer);

        const dec_msg1 = command_request.CommandRequest.decodeDelimited(reader);
        expect(dec_msg1.callbackIdx).toEqual(1);
        expect(dec_msg1.singleCommand?.requestType).toEqual(2);
        expect(dec_msg1.singleCommand?.argsArray?.args).toEqual(
            convertStringArrayToBuffer(["bar1", "bar2"]),
        );

        const dec_msg2 = command_request.CommandRequest.decodeDelimited(reader);
        expect(dec_msg2.callbackIdx).toEqual(3);
        expect(dec_msg2.singleCommand?.requestType).toEqual(4);
        expect(dec_msg2.singleCommand?.argsArray?.args).toEqual(
            convertStringArrayToBuffer(["bar3", "bar4"]),
        );
    });

    it.each([ProtocolVersion.RESP2, ProtocolVersion.RESP3])(
        "info without parameters",
        async (protocol) => {
            client = await GlideClient.createClient(
                getClientConfigurationOption(cluster.getAddresses(), protocol),
            );
            const result = await client.info();
            expect(intoString(result)).toEqual(
                expect.stringContaining("# Server"),
            );
            expect(intoString(result)).toEqual(
                expect.stringContaining("# Replication"),
            );
            expect(intoString(result)).toEqual(
                expect.not.stringContaining("# Latencystats"),
            );
        },
    );

    it.each([ProtocolVersion.RESP2, ProtocolVersion.RESP3])(
        "blocking timeout tests_%p",
        async (protocol) => {
            client = await GlideClient.createClient(
                getClientConfigurationOption(
                    cluster.getAddresses(),
                    protocol,
                    300,
                ),
            );

            const blmovePromise = client.blmove(
                "source",
                "destination",
                ListDirection.LEFT,
                ListDirection.LEFT,
                0.1,
            );
            const timeoutPromise = new Promise((resolve) => {
                setTimeout(resolve, 500);
            });

            try {
                await Promise.race([blmovePromise, timeoutPromise]);
            } finally {
                Promise.resolve(blmovePromise);
                client.close();
            }
        },
        5000,
    );

    it.each([ProtocolVersion.RESP2, ProtocolVersion.RESP3])(
        "select dbsize flushdb test %p",
        async (protocol) => {
            client = await GlideClient.createClient(
                getClientConfigurationOption(cluster.getAddresses(), protocol),
            );
            checkSimple(await client.select(0)).toEqual("OK");

            const key = uuidv4();
            const value = uuidv4();
            const result = await client.set(key, value);
            checkSimple(result).toEqual("OK");

            checkSimple(await client.select(1)).toEqual("OK");
            expect(await client.get(key)).toEqual(null);
            checkSimple(await client.flushdb()).toEqual("OK");
            expect(await client.dbsize()).toEqual(0);

            checkSimple(await client.select(0)).toEqual("OK");
            checkSimple(await client.get(key)).toEqual(value);

            expect(await client.dbsize()).toBeGreaterThan(0);
            checkSimple(await client.flushdb(FlushMode.SYNC)).toEqual("OK");
            expect(await client.dbsize()).toEqual(0);
        },
    );

    it.each([ProtocolVersion.RESP2, ProtocolVersion.RESP3])(
        `can send transactions_%p`,
        async (protocol) => {
            client = await GlideClient.createClient(
                getClientConfigurationOption(cluster.getAddresses(), protocol),
            );
            const transaction = new Transaction();
            const expectedRes = await transactionTest(
                transaction,
                cluster.getVersion(),
            );
            transaction.select(0);
            const result = await client.exec(transaction);
            expectedRes.push(["select(0)", "OK"]);

            validateTransactionResponse(result, expectedRes);
        },
    );

    it.each([ProtocolVersion.RESP2, ProtocolVersion.RESP3])(
        "can return null on WATCH transaction failures",
        async (protocol) => {
            const client1 = await GlideClient.createClient(
                getClientConfigurationOption(cluster.getAddresses(), protocol),
            );
            const client2 = await GlideClient.createClient(
                getClientConfigurationOption(cluster.getAddresses(), protocol),
            );
            const transaction = new Transaction();
            transaction.get("key");
            const result1 = await client1.customCommand(["WATCH", "key"]);
            expect(result1).toEqual("OK");

            const result2 = await client2.set("key", "foo");
            expect(result2).toEqual("OK");

            const result3 = await client1.exec(transaction);
            expect(result3).toBeNull();

            client1.close();
            client2.close();
        },
    );

    it.each([ProtocolVersion.RESP2, ProtocolVersion.RESP3])(
        "object freq transaction test_%p",
        async (protocol) => {
            const client = await GlideClient.createClient(
                getClientConfigurationOption(cluster.getAddresses(), protocol),
            );

            const key = uuidv4();
            const maxmemoryPolicyKey = "maxmemory-policy";
            const config = await client.configGet([maxmemoryPolicyKey]);
            const maxmemoryPolicy = String(config[maxmemoryPolicyKey]);

            try {
                const transaction = new Transaction();
                transaction.configSet({
                    [maxmemoryPolicyKey]: "allkeys-lfu",
                });
                transaction.set(key, "foo");
                transaction.objectFreq(key);

                const response = await client.exec(transaction);
                expect(response).not.toBeNull();

                if (response != null) {
                    expect(response.length).toEqual(3);
                    expect(response[0]).toEqual("OK");
                    expect(response[1]).toEqual("OK");
                    expect(response[2]).toBeGreaterThanOrEqual(0);
                }
            } finally {
                expect(
                    await client.configSet({
                        [maxmemoryPolicyKey]: maxmemoryPolicy,
                    }),
                ).toEqual("OK");
            }

            client.close();
        },
    );

    it.each([ProtocolVersion.RESP2, ProtocolVersion.RESP3])(
        "object idletime transaction test_%p",
        async (protocol) => {
            const client = await GlideClient.createClient(
                getClientConfigurationOption(cluster.getAddresses(), protocol),
            );

            const key = uuidv4();
            const maxmemoryPolicyKey = "maxmemory-policy";
            const config = await client.configGet([maxmemoryPolicyKey]);
            const maxmemoryPolicy = String(config[maxmemoryPolicyKey]);

            try {
                const transaction = new Transaction();
                transaction.configSet({
                    // OBJECT IDLETIME requires a non-LFU maxmemory-policy
                    [maxmemoryPolicyKey]: "allkeys-random",
                });
                transaction.set(key, "foo");
                transaction.objectIdletime(key);

                const response = await client.exec(transaction);
                expect(response).not.toBeNull();

                if (response != null) {
                    expect(response.length).toEqual(3);
                    // transaction.configSet({[maxmemoryPolicyKey]: "allkeys-random"});
                    expect(response[0]).toEqual("OK");
                    // transaction.set(key, "foo");
                    expect(response[1]).toEqual("OK");
                    // transaction.objectIdletime(key);
                    expect(response[2]).toBeGreaterThanOrEqual(0);
                }
            } finally {
                expect(
                    await client.configSet({
                        [maxmemoryPolicyKey]: maxmemoryPolicy,
                    }),
                ).toEqual("OK");
            }

            client.close();
        },
    );

    it.each([ProtocolVersion.RESP2, ProtocolVersion.RESP3])(
        "object refcount transaction test_%p",
        async (protocol) => {
            const client = await GlideClient.createClient(
                getClientConfigurationOption(cluster.getAddresses(), protocol),
            );

            const key = uuidv4();
            const transaction = new Transaction();
            transaction.set(key, "foo");
            transaction.objectRefcount(key);

            const response = await client.exec(transaction);
            expect(response).not.toBeNull();

            if (response != null) {
                expect(response.length).toEqual(2);
                expect(response[0]).toEqual("OK"); // transaction.set(key, "foo");
                expect(response[1]).toBeGreaterThanOrEqual(1); // transaction.objectRefcount(key);
            }

            client.close();
        },
    );

    it.each([ProtocolVersion.RESP2, ProtocolVersion.RESP3])(
        "lolwut test_%p",
        async (protocol) => {
            const client = await GlideClient.createClient(
                getClientConfigurationOption(cluster.getAddresses(), protocol),
            );

            const result = await client.lolwut();
            expect(intoString(result)).toEqual(
                expect.stringContaining("Redis ver. "),
            );

            const result2 = await client.lolwut({ parameters: [] });
            expect(intoString(result2)).toEqual(
                expect.stringContaining("Redis ver. "),
            );

            const result3 = await client.lolwut({ parameters: [50, 20] });
            expect(intoString(result3)).toEqual(
                expect.stringContaining("Redis ver. "),
            );

            const result4 = await client.lolwut({ version: 6 });
            expect(intoString(result4)).toEqual(
                expect.stringContaining("Redis ver. "),
            );

            const result5 = await client.lolwut({
                version: 5,
                parameters: [30, 4, 4],
            });
            expect(intoString(result5)).toEqual(
                expect.stringContaining("Redis ver. "),
            );

            // transaction tests
            const transaction = new Transaction();
            transaction.lolwut();
            transaction.lolwut({ version: 5 });
            transaction.lolwut({ parameters: [1, 2] });
            transaction.lolwut({ version: 6, parameters: [42] });
            const results = await client.exec(transaction);

            if (results) {
                for (const element of results) {
                    expect(intoString(element)).toEqual(
                        expect.stringContaining("Redis ver. "),
                    );
                }
            } else {
                throw new Error("Invalid LOLWUT transaction test results.");
            }

            client.close();
        },
        TIMEOUT,
    );

    it.each([ProtocolVersion.RESP2, ProtocolVersion.RESP3])(
        "function load test_%p",
        async (protocol) => {
            if (cluster.checkIfServerVersionLessThan("7.0.0")) return;

            const client = await GlideClient.createClient(
                getClientConfigurationOption(cluster.getAddresses(), protocol),
            );

            try {
                const libName = "mylib1C" + uuidv4().replaceAll("-", "");
                const funcName = "myfunc1c" + uuidv4().replaceAll("-", "");
                const code = generateLuaLibCode(
                    libName,
                    new Map([[funcName, "return args[1]"]]),
                    true,
                );
                expect(await client.functionList()).toEqual([]);

                checkSimple(await client.functionLoad(code)).toEqual(libName);

                checkSimple(
                    await client.fcall(funcName, [], ["one", "two"]),
                ).toEqual("one");
                checkSimple(
                    await client.fcallReadonly(funcName, [], ["one", "two"]),
                ).toEqual("one");

                let functionList = await client.functionList({
                    libNamePattern: libName,
                });
                let expectedDescription = new Map<string, string | null>([
                    [funcName, null],
                ]);
                let expectedFlags = new Map<string, string[]>([
                    [funcName, ["no-writes"]],
                ]);

                checkFunctionListResponse(
                    functionList,
                    libName,
                    expectedDescription,
                    expectedFlags,
                );

                // re-load library without replace

                await expect(client.functionLoad(code)).rejects.toThrow(
                    `Library '${libName}' already exists`,
                );

                // re-load library with replace
                checkSimple(await client.functionLoad(code, true)).toEqual(
                    libName,
                );

                // overwrite lib with new code
                const func2Name = "myfunc2c" + uuidv4().replaceAll("-", "");
                const newCode = generateLuaLibCode(
                    libName,
                    new Map([
                        [funcName, "return args[1]"],
                        [func2Name, "return #args"],
                    ]),
                    true,
                );
                checkSimple(await client.functionLoad(newCode, true)).toEqual(
                    libName,
                );

                functionList = await client.functionList({ withCode: true });
                expectedDescription = new Map<string, string | null>([
                    [funcName, null],
                    [func2Name, null],
                ]);
                expectedFlags = new Map<string, string[]>([
                    [funcName, ["no-writes"]],
                    [func2Name, ["no-writes"]],
                ]);

                checkFunctionListResponse(
                    functionList,
                    libName,
                    expectedDescription,
                    expectedFlags,
                    newCode,
                );

                checkSimple(
                    await client.fcall(func2Name, [], ["one", "two"]),
                ).toEqual(2);
                checkSimple(
                    await client.fcallReadonly(func2Name, [], ["one", "two"]),
                ).toEqual(2);
            } finally {
                expect(await client.functionFlush()).toEqual("OK");
                client.close();
            }
        },
    );

    it.each([ProtocolVersion.RESP2, ProtocolVersion.RESP3])(
        "function flush test_%p",
        async (protocol) => {
            if (cluster.checkIfServerVersionLessThan("7.0.0")) return;

            const client = await GlideClient.createClient(
                getClientConfigurationOption(cluster.getAddresses(), protocol),
            );

            try {
                const libName = "mylib1C" + uuidv4().replaceAll("-", "");
                const funcName = "myfunc1c" + uuidv4().replaceAll("-", "");
                const code = generateLuaLibCode(
                    libName,
                    new Map([[funcName, "return args[1]"]]),
                    true,
                );

                // verify function does not yet exist
                expect(await client.functionList()).toEqual([]);

                checkSimple(await client.functionLoad(code)).toEqual(libName);

                // Flush functions
                expect(await client.functionFlush(FlushMode.SYNC)).toEqual(
                    "OK",
                );
                expect(await client.functionFlush(FlushMode.ASYNC)).toEqual(
                    "OK",
                );

                // verify function does not yet exist
                expect(await client.functionList()).toEqual([]);

                // Attempt to re-load library without overwriting to ensure FLUSH was effective
                checkSimple(await client.functionLoad(code)).toEqual(libName);
            } finally {
                expect(await client.functionFlush()).toEqual("OK");
                client.close();
            }
        },
    );

    it.each([ProtocolVersion.RESP2, ProtocolVersion.RESP3])(
        "function delete test_%p",
        async (protocol) => {
            if (cluster.checkIfServerVersionLessThan("7.0.0")) return;

            const client = await GlideClient.createClient(
                getClientConfigurationOption(cluster.getAddresses(), protocol),
            );

            try {
                const libName = "mylib1C" + uuidv4().replaceAll("-", "");
                const funcName = "myfunc1c" + uuidv4().replaceAll("-", "");
                const code = generateLuaLibCode(
                    libName,
                    new Map([[funcName, "return args[1]"]]),
                    true,
                );
                // verify function does not yet exist
                expect(await client.functionList()).toEqual([]);

                checkSimple(await client.functionLoad(code)).toEqual(libName);

                // Delete the function
                expect(await client.functionDelete(libName)).toEqual("OK");

                // verify function does not yet exist
                expect(await client.functionList()).toEqual([]);

                // deleting a non-existing library
                await expect(client.functionDelete(libName)).rejects.toThrow(
                    `Library not found`,
                );
            } finally {
                expect(await client.functionFlush()).toEqual("OK");
                client.close();
            }
        },
    );

    runBaseTests<Context>({
        init: async (protocol, clientName?) => {
            const options = getClientConfigurationOption(
                cluster.getAddresses(),
                protocol,
            );
            options.protocol = protocol;
            options.clientName = clientName;
            testsFailed += 1;
            client = await GlideClient.createClient(options);
            return { client, context: { client }, cluster };
        },
        close: (context: Context, testSucceeded: boolean) => {
            if (testSucceeded) {
                testsFailed -= 1;
            }
        },
        timeout: TIMEOUT,
    });
});
