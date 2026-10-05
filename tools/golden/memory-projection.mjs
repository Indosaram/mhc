import { mkdirSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { execFileSync } from "node:child_process";

const root = process.env.OMO_SRC ?? "/home/indo/code/oh-my-openagent";
const pin = execFileSync("git", ["-C", root, "rev-parse", "HEAD"], { encoding: "utf8" }).trim();
if (pin !== "77f3067f157a4f88e6d8ed48b3a6c338654402ed") throw new Error("OMO source pin mismatch");
const { compileMemoryBlockAtRevision } = await import(pathToFileURL(resolve(root, "packages/memory-core/src/compile/compile.ts")).href);
const files = {
  "system/persona.md": "---\ndescription: Persona\n---\nfirst\n",
  "system/human.md": "---\ndescription: Human\n---\nfixture person\n",
  "notes/facts/example.md": "---\ndescription: Example\n---\nfixture fact\n",
};
const projection = await compileMemoryBlockAtRevision({
  lsTree: async () => Object.keys(files).sort(),
  show: async (_revision, path) => files[path],
}, "fixture-head", { agentId: "prompt-agent" });
const target = resolve(import.meta.dir, "../../crates/omo/components/maho-omo-memory/tests/golden");
mkdirSync(target, { recursive: true });
writeFileSync(resolve(target, "projection.txt"), projection);
console.log(`PASS pinned TS projection generated (${Buffer.byteLength(projection)} bytes)`);
