import { createInterface } from "node:readline";
const send = (value) => process.stdout.write(`${JSON.stringify({jsonrpc:"2.0",...value})}\n`);
let pending;
createInterface({input:process.stdin}).on("line", (line) => {
  const message = JSON.parse(line);
  if (message.method === "initialize") send({id:message.id,result:{protocolVersion:"2025-11-25",capabilities:{tools:{}},serverInfo:{name:"cancel-fixture",version:"1"}}});
  if (message.method === "tools/call") {
    pending = message.id;
    send({method:"notifications/progress",params:{progressToken:message.params._meta.progressToken,progress:1}});
  }
  if (message.method === "notifications/cancelled") send({method:"fixture/cancelled",params:{requestId:message.params.requestId,matched:message.params.requestId===pending}});
});
