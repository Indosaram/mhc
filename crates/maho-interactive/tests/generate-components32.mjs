import { pinnedSenpiRoot } from "../../../tools/golden/pin.mjs";
import { pathToFileURL } from "node:url";
import { writeFileSync } from "node:fs";
const root = pinnedSenpiRoot();
const { default: chalk } = await import(pathToFileURL(root + "/node_modules/chalk/source/index.js").href);
chalk.level = 1;
const prefix = root + "/packages/coding-agent/src/modes/interactive/";
const load = (name) => import(pathToFileURL(prefix + name + ".ts").href);
const { initTheme } = await load("theme/theme");
const { DynamicBorder } = await load("components/dynamic-border");
const { ContinuityNoticeTracker } = await load("components/continuity-notice");
const { UserMessageComponent } = await load("components/user-message");
const { BranchSummaryMessageComponent } = await load("components/branch-summary-message");
const { CompactionSummaryMessageComponent } = await load("components/compaction-summary-message");
const { SkillInvocationMessageComponent } = await load("components/skill-invocation-message");
initTheme("dark", false);
const cases = [
    { kind: "flatten", reason: "transcript_missing", payloadBytes: 214328, collapsedDirectives: 4 },
    { kind: "flatten", payloadBytes: 1024, collapsedDirectives: 0 },
    { kind: "flatten", reason: "registry_miss" },
    { kind: "flatten", payloadBytes: 1 },
    { kind: "flatten", payloadBytes: 2097152 },
    { kind: "disabled", reason: "resume_mode_off" },
    { kind: "delta" },
    { kind: "bootstrap" },
];
const messages = cases.map(details => ({ diagnostics: [{ type: "claude_sdk_oauth_session_continuity", details }] }));
messages.push({ diagnostics: [{ type: "claude_sdk_oauth_resume_fallback" }] }, {}, { diagnostics: [{ type: "other" }] });
const data = {
    borders: [0, 1, 10, 80].map(width => ({ width, lines: new DynamicBorder().render(width) })),
    continuity: messages.map(message => ({ message, notice: new ContinuityNoticeTracker().noticeFor(message) ?? null })),
    messages: [20, 40, 80].flatMap(width => [
        { kind: "user", width, text: "Hello **world**\n\n1. first\n2. second", lines: new UserMessageComponent("Hello **world**\n\n1. first\n2. second").render(width) },
        ...[false, true].map(expanded => {
            const component = new BranchSummaryMessageComponent({ summary: "Retained **context**." });
            component.setExpanded(expanded);
            return { kind: "branch", width, expanded, text: "Retained **context**.", lines: component.render(width) };
        }),
        ...[false, true].map(expanded => {
            const message = { summary: "Retained **context**.\r\n\u001b[31mSafe\u001b[0m", tokensBefore: 12345, details: { schema: "senpi.compaction.openai-remote.v1", transport: "websocket", retainedInputItemCount: 20, requestInputItemCount: 1000 } };
            const component = new CompactionSummaryMessageComponent(message);
            component.setExpanded(expanded);
            return { kind: "compaction", width, expanded, message, lines: component.render(width) };
        }),
        ...[false, true].map(expanded => {
            const skills = [{ name: "rust", content: "Use **safe** APIs." }, { name: "tests", content: "Run tests." }];
            const component = new SkillInvocationMessageComponent({ skills });
            component.setExpanded(expanded);
            return { kind: "skill", width, expanded, skills, lines: component.render(width) };
        }),
    ]),
};
writeFileSync(import.meta.dir + "/golden/components32-foundation.json", JSON.stringify(data, null, 2) + "\n");
console.log("Generated components32 foundation from pinned Senpi");
