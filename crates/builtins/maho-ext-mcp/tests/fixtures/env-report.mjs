import { createInterface } from "node:readline";
const report = Object.fromEntries(Object.entries(process.env).filter(([key]) => key.startsWith("MCP_ENV_")));
createInterface({ input: process.stdin }).on("line", (line) => {
  const message = JSON.parse(line);
  if (message.method !== "initialize") return;
  process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", id: message.id, result: {
    protocolVersion: "2025-11-25", capabilities: {},
    serverInfo: { name: "env-report", version: JSON.stringify(report) },
  } })}\n`);
});
