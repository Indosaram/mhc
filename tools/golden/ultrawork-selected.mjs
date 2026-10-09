import { execFileSync } from "node:child_process";
import { pathToFileURL } from "node:url";

export async function selectedUltraworkMessage(root, prompt) {
    const commit = execFileSync("git", ["-C", root, "rev-parse", "HEAD"], { encoding: "utf8" }).trim();
    if (commit !== "455dee623c9b2f2d2f1e9e68bf7b72a878ee3f0b") {
        throw new Error(`Selected OMO source mismatch: ${commit}`);
    }
    const { createUltraworkComponent, createSessionArming } = await import(pathToFileURL(`${root}/packages/omo-senpi/src/components/ultrawork/index.ts`).href);
    const { FakeExtensionAPI } = await import(pathToFileURL(`${root}/packages/omo-senpi/test-support/fake-extension-api.ts`).href);
    const api = new FakeExtensionAPI();
    await createUltraworkComponent(createSessionArming()).register(api, {
        logger: { info() {}, warn() {}, error() {} },
        config: { getFlag: name => api.getFlag(name) },
    });
    await api.dispatch("input", { type: "input", text: prompt, source: "interactive" }, {
        sessionManager: { getSessionId: () => "fixture" },
    });
    if (api.messages.length !== 1) throw new Error("Expected exactly one source injection");
    return api.messages[0].message;
}
