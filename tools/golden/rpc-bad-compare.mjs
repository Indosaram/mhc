#!/usr/bin/env bun
import { readFileSync } from "node:fs";
import { isDeepStrictEqual } from "node:util";
import { pinnedSenpiRoot } from "./pin.mjs";

pinnedSenpiRoot();
let error;
try { JSON.parse("{invalid"); } catch (cause) { error = `Failed to parse command: ${cause.message}`; }
const expected = { type: "response", command: "parse", success: false, error };
const actual = JSON.parse(readFileSync(process.argv[2], "utf8"));
if (isDeepStrictEqual(actual, expected)) console.log("PASS: malformed-line response equals senpi JSON.parse error frame");
else {
    console.error(`FAIL: malformed-line response expected ${JSON.stringify(expected)}; actual ${JSON.stringify(actual)}`);
    process.exitCode = 1;
}
