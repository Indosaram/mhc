import { writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { execFileSync } from "node:child_process";

const root = process.env.OMO_SRC ?? "/home/indo/code/oh-my-openagent";
const pin = execFileSync("git", ["-C", root, "rev-parse", "HEAD"], { encoding: "utf8" }).trim();
if (pin !== "77f3067f157a4f88e6d8ed48b3a6c338654402ed") throw new Error("OMO source pin mismatch");
const { parseMemoryFile } = await import(pathToFileURL(resolve(root, "packages/memory-core/src/memfs/frontmatter.ts")).href);
let error;
try { parseMemoryFile("broken persona without frontmatter\n"); } catch (failure) { error = failure.message; }
if (error === undefined) throw new Error("corrupted memory unexpectedly parsed");
writeFileSync(resolve(import.meta.dir, "../../crates/omo/components/maho-omo-memory/tests/golden/corrupt.txt"), `${error}\n`);
console.log("PASS pinned doctor frontmatter parser error generated");
