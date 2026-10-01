import { adversarialPrograms } from "/home/indo/code/senpi/packages/agent/test/harness/fixtures/read-summary/adversarial-grammar.ts";
import { adversarialSignatures, signatureSource } from "/home/indo/code/senpi/packages/agent/test/harness/fixtures/read-summary/adversarial-signatures.ts";
import { typescriptOracle } from "/home/indo/code/senpi/packages/agent/test/harness/fixtures/read-summary/oracle-typescript.ts";
import { selectedReadFolder, READ_FOLD_SETTINGS } from "/home/indo/code/senpi/packages/agent/src/harness/utils/read-folders/index.ts";
const cases = adversarialPrograms().map(program => ({ ...program, oracle: typescriptOracle(program.source, program.language), expected: selectedReadFolder.fold({path: `input.${program.language}`, text: program.source, settings: READ_FOLD_SETTINGS}) }));
const signatures = adversarialSignatures.map(fixture => ({...fixture, source: signatureSource(fixture), oracle: typescriptOracle(signatureSource(fixture), fixture.language)}));
if (process.argv.includes("--patterns")) {
    const object = `{\n${["alpha", "bravo", "charlie", "delta", "echo"].map(n => ` ${n}: "${n}",`).join("\n")}\n}`;
    const recovered = `(([value = ns.factory(${object})]) = source);`;
    const value = `({ value } = ns.factory(${object}));`;
    console.log(JSON.stringify({recovered:typescriptOracle(recovered,"ts"),value:typescriptOracle(value,"js")}));
} else if (process.argv.includes("--receipt")) {
    const { readFileSync } = await import("node:fs");
    const { createHash } = await import("node:crypto");
    const bytes = readFileSync("/home/indo/code/senpi/packages/agent/test/harness/fixtures/read-summary/selection.json");
    const selection = JSON.parse(bytes.toString());
    const sourceHashes = Object.fromEntries(Object.keys(selection.candidate_sources_sha256).map(path => [path, createHash("sha256").update(readFileSync(`/home/indo/code/senpi/packages/agent/src/harness/utils/${path}`)).digest("hex")]));
    console.log(JSON.stringify({selection, receiptSha256:createHash("sha256").update(bytes).digest("hex"), sourceHashes, grammarSha256:createHash("sha256").update(JSON.stringify(adversarialPrograms())).digest("hex")}));
} else console.log(JSON.stringify({cases, signatures}));
