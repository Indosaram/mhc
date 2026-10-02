import { createInterface } from "node:readline";
createInterface({ input: process.stdin }).on("line", (line) => {
  const message = JSON.parse(line);
  if (message.method !== "initialize") return;
  process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", id: message.id, result: {
    protocolVersion: "2025-11-25", capabilities: {},
    serverInfo: { name: "oauth-env", version: process.env.OAUTH_ACCESS_TOKEN === "fixture-current" ? "current" : "missing" },
  } })}\n`);
});
