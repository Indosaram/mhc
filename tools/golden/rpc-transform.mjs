#!/usr/bin/env bun
import { spawn } from "node:child_process";
import { resolve,join } from "node:path";
import { pinnedSenpiRoot } from "./pin.mjs";

const source = pinnedSenpiRoot();
const binary = process.argv[2];
if (!binary) throw new Error("Usage: bun tools/golden/rpc-transform.mjs <Rust protocol_transform binary>");
const { toJsonEvent } = await import(join(source,"packages/coding-agent/src/modes/json-event.ts"));
const { omitInlineMedia } = await import(join(source,"packages/coding-agent/src/modes/rpc/media-placeholders.ts"));
const { serializeJsonLine } = await import(join(source,"packages/coding-agent/src/modes/rpc/jsonl.ts"));
const records = [
    { type:"message_update",message:{role:"assistant",usage:{input:2,output:1}},assistantMessageEvent:{type:"text_delta",contentIndex:0,delta:"a\u2028b\u2029c",partial:{content:[]}} },
    { type:"message_update",message:{role:"assistant",usage:{}},assistantMessageEvent:{type:"toolcall_start",contentIndex:0,partial:{content:[{type:"toolCall",id:"call1",name:"bash"}]}} },
    { type:"tool_execution_end",toolCallId:"call1",result:{content:[{type:"image",mimeType:"image/png",data:"YQ=="},{type:"text",text:"ok"}]} },
    { type:"response",command:"get_messages",success:true,data:{messages:[{role:"toolResult",toolCallId:"call1",content:[{type:"image",data:"YWJj"}]}]} },
    { type:"message_end",message:{role:"user",content:[{type:"image",data:"YQ=="}]} },
];
const expected = records.map(record => omitInlineMedia(toJsonEvent(record)));
const child = spawn(resolve(binary),[],{stdio:["pipe","pipe","pipe"]});
const chunks = [], errors = [];
child.stdout.on("data",chunk => chunks.push(chunk));
child.stderr.on("data",chunk => errors.push(chunk));
const closed = new Promise((accept,reject) => {
    child.once("error",reject);
    child.once("close",(code,signal) => code === 0 ? accept() : reject(new Error(`Rust transform exited ${code}/${signal}: ${Buffer.concat(errors)}`)));
});
child.stdin.end(records.map(serializeJsonLine).join(""));
await closed;
const actual = Buffer.concat(chunks).toString("utf8").trimEnd().split("\n").map(line => JSON.parse(line));
if (JSON.stringify(actual) !== JSON.stringify(expected)) throw new Error("Rust JSONL transform differs from pinned Senpi output");
console.log(`PASS: ${actual.length} Rust JSONL records equal pinned Senpi transforms; child exited 0`);
