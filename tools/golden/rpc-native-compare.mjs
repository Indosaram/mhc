#!/usr/bin/env bun
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { isDeepStrictEqual } from "node:util";
import { pinnedSenpiRoot } from "./pin.mjs";

const [referencePath, nativePath] = process.argv.slice(2);
if (!referencePath || !nativePath) throw new Error("usage: rpc-native-compare.mjs <faux-harness.json> <native.jsonl>");
const source = pinnedSenpiRoot();
const { toJsonEvent } = await import(join(source, "packages/coding-agent/src/modes/json-event.ts"));
function normalize(value, key) {
    if (typeof value === "string" && /^(id|parentId|sessionId|entryId|responseId|toolCallId|turnKey)$/.test(key ?? "")) return "<id>";
    if (typeof value === "string" && key === "timestamp") return "<time>";
    if (typeof value === "number" && /^(timestamp|createdAt|updatedAt|startedAt|endedAt|durationMs|elapsedMs)$/.test(key ?? "")) return 0;
    if (Array.isArray(value)) return value.map((item) => normalize(item));
    if (value && typeof value === "object") return Object.fromEntries(Object.entries(value).map(([name, item]) => [name, normalize(item, name)]));
    return value;
}
const expected = JSON.parse(readFileSync(referencePath, "utf8")).events.map((event) => normalize(toJsonEvent(event)));
const actual = readFileSync(nativePath, "utf8").trimEnd().split("\n").map((line) => normalize(JSON.parse(line)));
if (isDeepStrictEqual(actual, expected)) {
    console.log(`PASS: ${actual.length} native JSONL records equal pinned faux-harness events`);
} else {
    console.error(`FAIL: native ${actual.length} records, pinned faux-harness ${expected.length} records`);
    const index = expected.findIndex((event, index) => !isDeepStrictEqual(event, actual[index]));
    console.error(`First divergent record ${index}: expected ${JSON.stringify(expected[index])}; actual ${JSON.stringify(actual[index])}`);
    const types = (events) => events.map((event) => event.type === "message_update" ? `${event.type}:${event.assistantMessageEvent.type}` : event.type);
    const expectedTypes = types(expected);
    const actualTypes = types(actual);
    const missing = expectedTypes.filter((type, index) => expectedTypes.slice(0, index + 1).filter((entry) => entry === type).length > actualTypes.filter((entry) => entry === type).length);
    const extra = actualTypes.filter((type, index) => actualTypes.slice(0, index + 1).filter((entry) => entry === type).length > expectedTypes.filter((entry) => entry === type).length);
    const assistantStart = (events) => events.find((event) => event.type === "message_start" && event.message?.role === "assistant")?.message;
    const deltas = (events) => events.filter((event) => event.type === "message_update" && event.assistantMessageEvent.type === "text_delta").map((event) => event.assistantMessageEvent.delta);
    console.error(JSON.stringify({
        expectedRecords: expected.length, actualRecords: actual.length, missing, extra,
        expectedAssistant: assistantStart(expected), actualAssistant: assistantStart(actual),
        expectedTextDeltas: deltas(expected), actualTextDeltas: deltas(actual),
    }));
    process.exitCode = 1;
}
