// SPDX-License-Identifier: MIT

import { test, describe, it } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import os from "node:os";
import { calculateSkillBytes } from "./skill-bytes.mjs";

describe("skill-bytes", () => {
  it("computes sizes on the current repo tree", async () => {
    const rootDir = path.resolve(import.meta.dirname, "../..");
    const result = await calculateSkillBytes(rootDir);

    assert.ok(result.skills["assets/omo-latest/skills/ultrawork/SKILL.md"] > 0);
    assert.ok(result.skills["assets/omo-latest/skills/ulw-loop/SKILL.md"] > 0);
    assert.ok(result.skills["assets/omo-latest/skills/ulw-execute/SKILL.md"] > 0);
    assert.ok(result.skills["assets/omo-latest/skills/ulw-plan/SKILL.md"] > 0);
    assert.ok(result.skills["assets/omo-latest/skills/ulw-research/SKILL.md"] > 0);

    assert.ok(result.contracts["crates/omo/components/maho-omo-ultrawork/src/directive.txt"] === 31542);

    assert.ok(result.totalSkillBytes > 0);
    assert.ok(result.totalContractBytes > 0);
    assert.equal(result.totalBytes, result.totalSkillBytes + result.totalContractBytes);
  });

  it("computes sizes on a synthetic root directory", async () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "ulw-test-skills-"));
    try {
      const ultraworkDir = path.join(tmpDir, "assets/omo-latest/skills/ultrawork");
      fs.mkdirSync(ultraworkDir, { recursive: true });
      fs.writeFileSync(path.join(ultraworkDir, "SKILL.md"), "hello ultrawork\n", "utf8");

      const ulwPlanRefsDir = path.join(tmpDir, "assets/omo-latest/skills/ulw-plan/references");
      fs.mkdirSync(ulwPlanRefsDir, { recursive: true });
      fs.writeFileSync(path.join(ulwPlanRefsDir, "intent.md"), "plan reference\n", "utf8");

      const contractDir = path.join(tmpDir, "crates/maho-ultrawork/contract");
      fs.mkdirSync(contractDir, { recursive: true });
      fs.writeFileSync(path.join(contractDir, "core.md"), "contract core\n", "utf8");

      const result = await calculateSkillBytes(tmpDir);

      assert.equal(result.skills["assets/omo-latest/skills/ultrawork/SKILL.md"], 16);
      assert.equal(result.skills["assets/omo-latest/skills/ulw-plan/references/intent.md"], 15);
      assert.equal(result.contracts["crates/maho-ultrawork/contract/core.md"], 14);

      assert.equal(result.totalSkillBytes, 31);
      assert.equal(result.totalContractBytes, 14);
      assert.equal(result.totalBytes, 45);
    } finally {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    }
  });
});
