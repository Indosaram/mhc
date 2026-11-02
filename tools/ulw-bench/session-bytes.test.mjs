// SPDX-License-Identifier: MIT

import { test, describe, it } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import os from "node:os";
import { execFileSync } from "node:child_process";
import {
  parseSessionBytesFromLines,
  calculateSessionBytes,
} from "./session-bytes.mjs";

describe("session-bytes", () => {
  it("computes byte aggregations on a 3-line synthetic jsonl", () => {
    const line1 = JSON.stringify({
      type: "custom_message",
      customType: "omo-ultrawork:directive",
      content: [{ type: "text", text: "directive text" }],
    });
    const line2 = JSON.stringify({
      type: "custom_message",
      customType: "omo-senpi:wake",
      content: [{ type: "text", text: "wake payload" }],
    });
    const line3 = JSON.stringify({
      type: "message",
      message: { role: "toolResult", content: "tool output" },
    });

    const lines = [line1, line2, line3];
    const result = parseSessionBytesFromLines(lines);

    assert.equal(result.directive, line1.length);
    assert.equal(result.wake, line2.length);
    assert.equal(result.toolResult, line3.length);
    assert.equal(result.reminder, 0);
    assert.equal(result.assistant, 0);
    assert.equal(result.perTurnInjected, line1.length);
  });

  it("handles user turns and reminders in synthetic entries", () => {
    const lines = [
      JSON.stringify({ type: "message", message: { role: "user", content: "first prompt" } }),
      JSON.stringify({ type: "custom_message", customType: "omo-ultrawork:directive", content: "dir" }),
      JSON.stringify({ type: "custom_message", customType: "senpi.todo-first-turn", content: "rem" }),
      JSON.stringify({ type: "message", message: { role: "assistant", content: "reply" } }),
      JSON.stringify({ type: "message", message: { role: "user", content: "second prompt" } }),
    ];

    const result = parseSessionBytesFromLines(lines);
    assert.equal(result.directive, lines[1].length);
    assert.equal(result.reminder, lines[2].length);
    assert.equal(result.assistant, lines[3].length);
    // 2 user turns, perTurnInjected = round(directive / 2)
    assert.equal(result.perTurnInjected, Math.round(lines[1].length / 2));
  });

  it("reproduces wake bytes within 1% of 266,872 on real session snapshot", async () => {
    const fixturePath = path.join(import.meta.dirname, "fixtures/session-01a197b4-wake6.jsonl");
    assert.ok(fs.existsSync(fixturePath), "snapshot fixture exists");

    const result = await calculateSessionBytes(fixturePath);

    assert.ok(typeof result.wake === "number");
    const diff = Math.abs(result.wake - 266872);
    const diffPct = (diff / 266872) * 100;
    assert.ok(diffPct < 1.0, `Wake diff ${diffPct.toFixed(3)}% must be within 1% of 266,872`);
    assert.equal(result.wake, 266872, "Wake bytes exact match");

    assert.equal(result.directive, 32184);
    assert.equal(result.reminder, 668);
    assert.equal(result.toolResult, 113334);
    assert.equal(result.assistant, 26456);
    assert.equal(result.perTurnInjected, 32184);
  });

  it("reports line number and fails on malformed jsonl", () => {
    const lines = [
      JSON.stringify({ type: "message", message: { role: "user", content: "valid" } }),
      "{ this is not valid json }",
      JSON.stringify({ type: "message", message: { role: "assistant", content: "valid" } }),
    ];

    assert.throws(
      () => parseSessionBytesFromLines(lines),
      (err) => {
        assert.ok(err.message.includes("Malformed JSON at line 2"));
        assert.equal(err.lineNumber, 2);
        return true;
      }
    );
  });

  it("CLI exits with code 2 on malformed file", () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "ulw-test-malformed-"));
    const tmpFile = path.join(tmpDir, "bad.jsonl");
    fs.writeFileSync(tmpFile, '{"valid": true}\n{bad json}\n', "utf8");

    try {
      execFileSync(
        process.execPath,
        [path.join(import.meta.dirname, "session-bytes.mjs"), tmpFile],
        { stdio: "pipe" }
      );
      assert.fail("Should have exited with code 2");
    } catch (err) {
      assert.equal(err.status, 2, "Exit code must be 2 for malformed jsonl");
      const stderr = err.stderr.toString("utf8");
      assert.ok(stderr.includes("Malformed JSON at line 2"));
    } finally {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    }
  });
});
