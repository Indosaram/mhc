// SPDX-License-Identifier: MIT

import fs from "node:fs";
import path from "node:path";

const SKILL_NAMES = ["ultrawork", "ulw-loop", "ulw-execute", "ulw-plan", "ulw-research"];

/**
 * Collects all .md files under a references directory.
 * @param {string} referencesDir
 * @returns {string[]}
 */
function collectReferenceFiles(referencesDir) {
  if (!fs.existsSync(referencesDir)) {
    return [];
  }
  const entries = fs.readdirSync(referencesDir, { withFileTypes: true });
  const results = [];
  for (const entry of entries) {
    const fullPath = path.join(referencesDir, entry.name);
    if (entry.isFile() && entry.name.endsWith(".md")) {
      results.push(fullPath);
    } else if (entry.isDirectory()) {
      results.push(...collectReferenceFiles(fullPath));
    }
  }
  return results.sort();
}

/**
 * Calculates byte sizes of SKILL.md, references, and contract files.
 * @param {string} rootDir
 * @returns {{ skills: Record<string, number>, contracts: Record<string, number>, totalSkillBytes: number, totalContractBytes: number, totalBytes: number }}
 */
export async function calculateSkillBytes(rootDir = process.cwd()) {
  const resolvedRoot = path.resolve(rootDir);
  const skillsMap = {};
  const contractsMap = {};
  let totalSkillBytes = 0;
  let totalContractBytes = 0;

  const skillsBaseDir = path.join(resolvedRoot, "assets/omo-latest/skills");

  for (const skillName of SKILL_NAMES) {
    const skillDir = path.join(skillsBaseDir, skillName);
    if (!fs.existsSync(skillDir)) {
      continue;
    }

    const skillMd = path.join(skillDir, "SKILL.md");
    if (fs.existsSync(skillMd)) {
      const relPath = path.relative(resolvedRoot, skillMd);
      const size = fs.statSync(skillMd).size;
      skillsMap[relPath] = size;
      totalSkillBytes += size;
    }

    const referencesDir = path.join(skillDir, "references");
    const refFiles = collectReferenceFiles(referencesDir);
    for (const refFile of refFiles) {
      const relPath = path.relative(resolvedRoot, refFile);
      const size = fs.statSync(refFile).size;
      skillsMap[relPath] = size;
      totalSkillBytes += size;
    }
  }

  // Check candidate contract file paths
  const candidateContracts = [
    path.join(resolvedRoot, "crates/maho-ultrawork/contract/core.md"),
    path.join(resolvedRoot, "crates/omo/components/maho-omo-ultrawork/src/directive.txt"),
  ];

  const mahoContractRefsDir = path.join(resolvedRoot, "crates/maho-ultrawork/contract/references");
  if (fs.existsSync(mahoContractRefsDir)) {
    for (const ref of collectReferenceFiles(mahoContractRefsDir)) {
      candidateContracts.push(ref);
    }
  }

  for (const contractPath of candidateContracts) {
    if (fs.existsSync(contractPath)) {
      const relPath = path.relative(resolvedRoot, contractPath);
      const size = fs.statSync(contractPath).size;
      contractsMap[relPath] = size;
      totalContractBytes += size;
    }
  }

  return {
    skills: skillsMap,
    contracts: contractsMap,
    totalSkillBytes,
    totalContractBytes,
    totalBytes: totalSkillBytes + totalContractBytes,
  };
}

async function main() {
  const args = process.argv.slice(2);
  let rootDir = process.cwd();

  for (let i = 0; i < args.length; i++) {
    if (args[i] === "--root" && i + 1 < args.length) {
      rootDir = args[i + 1];
      i++;
    } else if (args[i] === "--help" || args[i] === "-h") {
      console.log("Usage: node skill-bytes.mjs [--root <dir>]");
      process.exit(0);
    }
  }

  const result = await calculateSkillBytes(rootDir);
  console.log(JSON.stringify(result, null, 2));
}

if (process.argv[1] && path.resolve(process.argv[1]) === path.resolve(import.meta.filename || "")) {
  main().catch((err) => {
    console.error(err);
    process.exit(1);
  });
}
