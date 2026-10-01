#!/usr/bin/env bun
// Golden fixture generator for agent_pb.rs wire-format parity, scoped to
// crates/maho-ai/src/api/cursor_agent/gen/ per todo 12-cursor-pb. Encodes
// representative agent.v1 messages with pinned senpi's protoc-gen-es client
// (packages/ai/src/api/cursor-agent/gen/agent_pb.ts) and writes the raw
// protobuf bytes into testdata/*.bin. tests.rs decodes each fixture with the
// prost-generated Rust type, re-encodes it, and asserts the bytes match
// byte-for-byte. Never hand-edit testdata/*.bin; regenerate with:
//
//   bun crates/maho-ai/src/api/cursor_agent/gen/golden_gen.mjs
//
// Case selection covers the wire-format shapes agent_pb consumers rely on:
// plain scalars (Position), 64-bit + repeated string (TodoItem), a map field
// (UpdateEnvironmentVariablesRequest), a oneof (GlobToolResult), a bytes
// field paired with a nested message (GlobToolCall), and a message with many
// proto3 `optional` (explicit-presence) fields plus a nested message
// (ShellArgs).
import { create, toBinary } from "@bufbuild/protobuf";
import { execFileSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const SENPI_PIN = "fe8c564bf33a2cbbdbbba99c9bd8b45b21e37407";

function pinnedSenpiRoot() {
	const root = resolve(process.env.SENPI_SRC ?? "/Users/indo/code/senpi");
	let head;
	try {
		head = execFileSync("git", ["-C", root, "rev-parse", "HEAD"], { encoding: "utf8" }).trim();
	} catch (error) {
		console.error(`senpi: ${root} is not a git checkout (${error.message.split("\n")[0]})`);
		process.exit(3);
	}
	if (head !== SENPI_PIN) {
		console.error(`senpi: ${root} is at ${head}, expected ${SENPI_PIN}; refusing to generate fixtures`);
		process.exit(3);
	}
	return root;
}

const here = dirname(fileURLToPath(import.meta.url));
const outDir = join(here, "testdata");

async function main() {
	const senpiRoot = pinnedSenpiRoot();
	const genPath = join(senpiRoot, "packages/ai/src/api/cursor-agent/gen/agent_pb.ts");
	const {
		PositionSchema,
		TodoItemSchema,
		UpdateEnvironmentVariablesRequestSchema,
		GlobToolResultSchema,
		GlobToolCallSchema,
		ShellArgsSchema,
	} = await import(genPath);

	mkdirSync(outDir, { recursive: true });

	const cases = [
		{
			name: "position",
			schema: PositionSchema,
			value: { line: 42, column: 7 },
		},
		{
			name: "todo-item",
			schema: TodoItemSchema,
			value: {
				id: "abc",
				content: "do the thing",
				status: 2,
				createdAt: 1700000000000n,
				updatedAt: 1700000001000n,
				dependencies: ["x", "y"],
			},
		},
		{
			name: "update-environment-variables-request",
			schema: UpdateEnvironmentVariablesRequestSchema,
			// Map entries are written in the order they appear here (protobuf-es
			// preserves insertion order). The Rust side decodes them into a
			// BTreeMap, so the keys must already be sorted for the re-encoded
			// bytes to match byte-for-byte.
			value: { env: { BAZ: "qux", FOO: "bar" }, replace: true },
		},
		{
			name: "glob-tool-result-success",
			schema: GlobToolResultSchema,
			value: {
				result: {
					case: "success",
					value: {
						pattern: "*.ts",
						path: "/src",
						files: ["a.ts", "b.ts"],
						totalFiles: 2,
						clientTruncated: false,
						ripgrepTruncated: true,
					},
				},
			},
		},
		{
			name: "glob-tool-result-error",
			schema: GlobToolResultSchema,
			value: { result: { case: "error", value: { error: "pattern too broad" } } },
		},
		{
			name: "glob-tool-call",
			schema: GlobToolCallSchema,
			value: {
				args: new TextEncoder().encode('{"pattern":"*.ts"}'),
				result: { result: { case: "error", value: { error: "boom" } } },
			},
		},
		{
			name: "shell-args",
			schema: ShellArgsSchema,
			value: {
				command: "ls -la",
				workingDirectory: "/tmp",
				timeout: 30,
				toolCallId: "tc-1",
				simpleCommands: ["ls"],
				hasInputRedirect: false,
				hasOutputRedirect: false,
				requestedSandboxPolicy: { type: 2, networkAccess: true },
				fileOutputThresholdBytes: 4096n,
				isBackground: false,
				skipApproval: true,
				timeoutBehavior: 1,
				hardTimeout: 60000,
				description: "list files",
				closeStdin: false,
				conversationId: "conv-1",
			},
		},
	];

	const manifest = [];
	for (const { name, schema, value } of cases) {
		const message = create(schema, value);
		const bytes = toBinary(schema, message);
		writeFileSync(join(outDir, `${name}.bin`), bytes);
		manifest.push(name);
		console.log(`wrote testdata/${name}.bin (${bytes.length} bytes)`);
	}
	writeFileSync(join(outDir, "manifest.json"), `${JSON.stringify(manifest, null, "\t")}\n`);
}

await main();
