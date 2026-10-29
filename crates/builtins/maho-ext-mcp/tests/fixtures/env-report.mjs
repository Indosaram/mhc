import { createInterface } from "node:readline";
createInterface({ input: process.stdin }).on("line", (line) => {
  const message = JSON.parse(line);
  if (message.method !== "initialize") return;
  process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", id: message.id, result: { env: Object.keys(process.env) } })}\n`);
});
