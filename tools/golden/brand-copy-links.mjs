import { cpSync, lstatSync, mkdirSync, mkdtempSync, readlinkSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const root = mkdtempSync(join(tmpdir(), "maho-brand-link-source-"));
try {
  const source = join(root, "source");
  const target = join(root, "target");
  mkdirSync(source);
  writeFileSync(join(root, "external"), "fixture");
  symlinkSync("../external", join(source, "file"));
  symlinkSync(".", join(source, "cycle"));
  cpSync(source, target, { recursive: true, errorOnExist: false });
  const result = {
    fileIsLink: lstatSync(join(target, "file")).isSymbolicLink(),
    fileTargetMatches: realpathSync(join(target, "file")) === join(root, "external"),
    cycleIsLink: lstatSync(join(target, "cycle")).isSymbolicLink(),
    cycleTargetMatches: realpathSync(join(target, "cycle")) === source,
    absoluteTarget: readlinkSync(join(target, "file")).startsWith("/"),
  };
  if (!Object.values(result).every(Boolean)) throw new Error(JSON.stringify(result));
  console.log(JSON.stringify(result));
} finally {
  rmSync(root, { recursive: true });
}
