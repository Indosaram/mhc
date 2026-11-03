// SPDX-License-Identifier: MIT

import fs from "node:fs";
import path from "node:path";

/**
 * Checks whether an entry matches the directive custom type.
 * @param {string|undefined} customType
 * @returns {boolean}
 */
export function isDirectiveType(customType) {
  return (
    customType === "omo-ultrawork:directive" ||
    customType === "maho-ultrawork:directive" ||
    customType === "ultrawork:directive"
  );
}

/**
 * Checks whether an entry matches the reminder custom type.
 * @param {string|undefined} customType
 * @returns {boolean}
 */
export function isReminderType(customType) {
  return (
    customType === "senpi.todo-first-turn" ||
    customType === "senpi.todo-owed" ||
    customType === "maho-ultrawork:reminder" ||
    customType === "omo-todo-fanout-reminder" ||
    (typeof customType === "string" && customType.includes("reminder"))
  );
}

/**
 * Checks whether an entry matches the wake custom type.
 * @param {string|undefined} customType
 * @returns {boolean}
 */
export function isWakeType(customType) {
  return (
    customType === "omo-senpi:wake" ||
    customType === "maho-senpi:wake" ||
    customType === "task:wake"
  );
}

/**
 * Splits one wake message into its per-child completions. One wake batch can carry several
 * children; each child's report is the text after its `result:` marker.
 * @param {string} text
 * @returns {{ id: string|null, status: string|null, reportChars: number }[]}
 */
export function parseWakeChildren(text) {
  return text
    .split(/(?=task completion name:)/)
    .filter((part) => part.startsWith("task completion name:"))
    .map((part) => {
      const at = part.indexOf("\nresult:");
      const report = at === -1 ? "" : part.slice(at + "\nresult:".length).replace(/^"|"\s*$/g, "");
      return {
        id: /\bid:(\S+)/.exec(part)?.[1] ?? null,
        status: /\bstatus:(\S+)/.exec(part)?.[1] ?? null,
        reportChars: report.length,
      };
    });
}

/**
 * Parses lines from a session JSONL file and computes byte aggregations.
 * @param {Iterable<string>} lines
 * @returns {{ directive: number, reminder: number, wake: number, toolResult: number, assistant: number, perTurnInjected: number }}
 */
export function parseSessionBytesFromLines(lines) {
  let directive = 0;
  let reminder = 0;
  let wake = 0;
  let toolResult = 0;
  let assistant = 0;
  let userTurns = 0;
  const wakeBatches = [];

  let lineNumber = 0;
  for (const line of lines) {
    lineNumber++;
    const trimmed = line.trim();
    if (!trimmed) {
      continue;
    }

    let obj;
    try {
      obj = JSON.parse(line);
    } catch (err) {
      const error = new Error(`Malformed JSON at line ${lineNumber}: ${err.message}`);
      error.lineNumber = lineNumber;
      throw error;
    }

    if (obj.type === "custom_message") {
      const customType = obj.customType;
      if (isDirectiveType(customType)) {
        directive += line.length;
      } else if (isReminderType(customType)) {
        reminder += line.length;
      } else if (isWakeType(customType)) {
        wake += line.length;
        const text = (Array.isArray(obj.content) ? obj.content : []).map((c) => c?.text ?? "").join("");
        wakeBatches.push({ bytes: line.length, children: parseWakeChildren(text) });
      }
    } else if (obj.type === "message") {
      const role = obj.message?.role;
      if (role === "toolResult") {
        toolResult += line.length;
      } else if (role === "assistant") {
        assistant += line.length;
      } else if (role === "user") {
        userTurns++;
      }
    }
  }

  const turns = Math.max(1, userTurns);
  const perTurnInjected = Math.round(directive / turns);

  return {
    directive,
    reminder,
    wake,
    toolResult,
    assistant,
    perTurnInjected,
    wakeBatches,
    childReportChars: wakeBatches.flatMap((b) => b.children.map((c) => c.reportChars)),
  };
}

/**
 * Calculates session bytes from a JSONL file on disk.
 * @param {string} filePath
 * @returns {Promise<ReturnType<typeof parseSessionBytesFromLines>>}
 */
export async function calculateSessionBytes(filePath) {
  return parseSessionBytesFromLines(fs.readFileSync(filePath, "utf8").split("\n"));
}

async function main() {
  const args = process.argv.slice(2);
  if (args.length === 0 || args.includes("--help") || args.includes("-h")) {
    console.error("Usage: node session-bytes.mjs <session.jsonl>");
    process.exit(1);
  }

  const filePath = path.resolve(process.cwd(), args[0]);
  if (!fs.existsSync(filePath)) {
    console.error(`Error: file not found: ${filePath}`);
    process.exit(1);
  }

  try {
    const stats = await calculateSessionBytes(filePath);
    console.log(JSON.stringify(stats, null, 2));
  } catch (err) {
    if (err.lineNumber) {
      console.error(err.message);
      process.exit(2);
    }
    console.error(`Error reading session file: ${err.message}`);
    process.exit(1);
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === path.resolve(import.meta.filename || "")) {
  main().catch((err) => {
    console.error(err);
    process.exit(1);
  });
}
