// Run under Node: the prebuilt node-pty addon supplies real Windows ConPTY.
import { createRequire } from "node:module";
import { createInterface } from "node:readline";

const require = createRequire(process.env.MHC_QA_NODE_PTY_MODULE);
const { spawn } = require("@lydell/node-pty");
const { binary, args, cwd, env } = JSON.parse(process.argv[2]);
const input = createInterface({ input: process.stdin });
const terminal = spawn(binary, args, {
	name: "xterm-256color", cols: 100, rows: 32, cwd, env,
	useConpty: true,
});
const send = record => process.stdout.write(`${JSON.stringify(record)}\n`);
send({ pid: terminal.pid });
terminal.onData(data => send({ data }));
terminal.onExit(({ exitCode }) => {
	send({ exitCode });
	process.exitCode = exitCode === 0 ? 0 : 1;
	input.close();
	process.stdin.destroy();
});
input.on("line", line => {
	const command = JSON.parse(line);
	if (command.kill) terminal.kill();
	else terminal.write(command.data);
});
input.on("close", () => { if (process.exitCode === undefined) terminal.kill(); });
