import { mkdtempSync, mkdirSync, rmSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { pinnedSenpiRoot } from "../../../../tools/golden/pin.mjs";
import { isolatedEnv } from "../../../../tools/golden/faux-common.mjs";

// Re-exec without inherited credentials or agent directories. Clean only this
// invocation's directory; do not use the shared stale-home sweep.
if (process.env.MAHO_FAUX_ISOLATED !== "1") {
  const home = mkdtempSync("/tmp/compaction-source-");
  mkdirSync(join(home, "tmp"));
  try {
    const child = Bun.spawn([process.execPath, import.meta.filename], {
      env: isolatedEnv(home), cwd: home, stdout: "inherit", stderr: "inherit",
    });
    process.exitCode = await child.exited;
  } finally { rmSync(home, { recursive: true, force: true }); }
} else {
  const root = pinnedSenpiRoot();
  const load = (path) => import(pathToFileURL(`${root}/${path}`).href);
  const { createHarness } = await load("packages/coding-agent/test/suite/harness.ts");
  const { default: compaction } = await load("packages/coding-agent/src/core/extensions/builtin/compaction/index.ts");
  const { fauxAssistantMessage } = await load("packages/ai/src/providers/faux.ts");
  const harness = await createHarness({
    settings: { compaction: { keepRecentTokens: 1, speculativeEnabled: false, idleCompactionEnabled: false } },
    extensionFactories: [(pi) => compaction(pi)], autoTitleSessions: false,
  });
  try {
    const model = harness.getModel();
    harness.sessionManager.appendMessage({ role: "user", content: "old ".repeat(30000), timestamp: 0 });
    harness.sessionManager.appendMessage({ ...fauxAssistantMessage("reply", { timestamp: 0 }), api: model.api, provider: model.provider, model: model.id });
    harness.sessionManager.appendMessage({ role: "user", content: "continue", timestamp: 0 });
    harness.setResponses([fauxAssistantMessage("<summary>native checkpoint</summary>", { timestamp: 0 })]);
    const result = await harness.session.compact();
    console.log(JSON.stringify({
      summary: result.summary,
      tokensBefore: result.tokensBefore,
      usage: result.usage,
      lifecycle: harness.events.filter((event) => event.type === "compaction_start" || event.type === "compaction_end")
        .map((event) => ({ type: event.type, reason: event.reason, ...(event.type === "compaction_end" ? { accepted: event.accepted, aborted: event.aborted } : {}) })),
      compactions: harness.sessionManager.getEntries().filter((entry) => entry.type === "compaction").length,
      requests: harness.faux.getCallLog().map(({ context }) => ({
        roles: context.messages.map((message) => message.role),
      })),
    }));
  } finally { harness.cleanup(); }
}
