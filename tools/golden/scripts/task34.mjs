import { mkdirSync, writeFileSync } from "node:fs";
import { resolve, join } from "node:path";
import { pathToFileURL } from "node:url";
import { pinnedSenpiRoot } from "../pin.mjs";

process.env.SENPI_BRAND = JSON.stringify({ name: "maho", configDir: ".maho", envPrefix: "MAHO" });
process.env.COLORTERM = "truecolor";
process.env.FORCE_COLOR = "3";

const root = pinnedSenpiRoot();
const source = "packages/coding-agent/src/modes/interactive/";
const load = (path) => import(pathToFileURL(join(root, path)).href);
const dir = resolve(import.meta.dir, "../../../crates/maho-interactive/tests/golden");
mkdirSync(dir, { recursive: true });
const write = (file, data) => {
	writeFileSync(join(dir, file), data);
	console.log(`wrote ${file}`);
};

const themeModule = await load(source + "theme/theme.ts");
themeModule.initTheme("dark", false);

const coreKeybindings = await load("packages/coding-agent/src/core/keybindings.ts");
const { setKeybindings, getKeybindings } = await import(
	pathToFileURL(join(root, "packages/tui/src/keybindings.ts")).href
);
setKeybindings(new coreKeybindings.KeybindingsManager({}, undefined));

const { AskUserQuestionComponent } = await load(source + "components/ask-user-question.ts");
const { formatCountdownLabel, NOT_ANSWERED_NOTICE } = await load(source + "components/ask-user-question-state.ts");
const { buildTimedOutResponse, buildCommentResponse, unansweredIds, renderStatusLine } = await load(
	source + "components/ask-user-async-widget.ts",
);

const request = {
	requestId: "q1",
	questions: [
		{
			id: "framework",
			header: "Framework",
			question: "Which framework should the port use?",
			options: [{ label: "React" }, { label: "Vue" }, { label: "Svelte" }],
			multiSelect: false,
		},
	],
	waitForAnswer: true,
	timeoutMs: 60000,
};

const draft = { answers: {}, comment: undefined };

// Happy path: arrow-down then Enter resolves the single-question request on option 2.
let response;
const component = new AskUserQuestionComponent(request, (value) => {
	response = value;
});
component.handleInput("\x1b[B");
component.handleInput("\r");

const happy = {
	keys: ["down", "enter"],
	request,
	response,
};

// Empty-submit guard: Enter on the Submit tab with no answer keeps the overlay open and notices.
let noticeResponse;
const noticeComponent = new AskUserQuestionComponent(request, (value) => {
	noticeResponse = value;
});
noticeComponent.handleInput("c");
noticeComponent.handleInput("\r");
const noticeState = noticeComponent.state;
const notice = {
	notice: noticeState.notice,
	expected: NOT_ANSWERED_NOTICE,
	resolved: noticeResponse !== undefined,
};

const timeout = {
	autoResolvedAfterMs: 60000,
	response: buildTimedOutResponse(request, draft, 60000),
	commentResponse: buildCommentResponse(request, draft, "use react please"),
	unanswered: unansweredIds(request, draft),
	statusLine: renderStatusLine(1, "01:00", 1),
	statusLineMulti: renderStatusLine(2, "05:00", 3),
	countdownLabels: [0, 999, 1000, 59000, 60000, 299000, 300000, 600000].map((ms) => [
		ms,
		formatCountdownLabel(ms),
	]),
};

write("task34-ask.json", JSON.stringify({ happy, notice, timeout }, null, 2) + "\n");

// Tips: registry order, scheduler selection and the favorite-cycle status messages.
const registry = await load(source + "tips/registry.ts");
const scheduler = await load(source + "tips/scheduler.ts");
const startupTip = await load(source + "tips/startup-tip.ts");
const workingTip = await load(source + "tips/working-tip.ts");
const favoriteMessages = await load(source + "tips/favorite-messages.ts");
const historyWriter = await load(source + "tips/history-writer.ts");
const keys = (binding) => {
	const list = getKeybindings().getKeys(binding);
	return list.length === 0 ? "" : list.join("/");
};
const hasCommand = (command) => ["help", "diff", "memory", "tasks", "fallback"].includes(command);

const selections = [
	{ history: {}, exclude: [], hasCommand: undefined },
	{ history: { "thinking-level": 10 }, exclude: [], hasCommand: undefined },
	{ history: { "thinking-level": 10, "favorite-model-rotation": 5 }, exclude: [], hasCommand: undefined },
	{
		history: Object.fromEntries(registry.TIP_DEFINITIONS.map((tip) => [tip.id, 1])),
		exclude: [],
		hasCommand,
	},
];

const tips = {
	registryIds: registry.TIP_DEFINITIONS.map((tip) => tip.id),
	keys: {
		"app.thinking.cycle": keys("app.thinking.cycle"),
		"app.model.select": keys("app.model.select"),
		"app.models.toggleFavorite": keys("app.models.toggleFavorite"),
	},
	selections: selections.map((entry) => ({
		history: entry.history,
		hasCommand: entry.hasCommand !== undefined,
		tipId:
			scheduler.selectTip(registry.TIP_DEFINITIONS, entry.history, 0, {
				exclude: new Set(entry.exclude),
				keys,
				...(entry.hasCommand ? { hasCommand: entry.hasCommand } : {}),
			})?.id ?? null,
	})),
	startupTip: startupTip.resolveStartupTipLine({
		tipsEnabled: true,
		quietStartup: false,
		history: { "thinking-level": 10 },
		now: 0,
		definitions: registry.TIP_DEFINITIONS,
		keys,
	}),
	startupTipDisabled: startupTip.resolveStartupTipLine({
		tipsEnabled: false,
		quietStartup: false,
		history: {},
		now: 0,
		definitions: registry.TIP_DEFINITIONS,
		keys,
	}),
	workingTip: workingTip.resolveWorkingTipLine({
		tipsEnabled: true,
		history: {},
		sessionShownTipIds: new Set(["thinking-level"]),
		now: 0,
		definitions: registry.TIP_DEFINITIONS,
		keys,
	}),
	historyRecorded: historyWriter.recordTipShown({ a: 1 }, "b", 7),
	favoriteEmpty: favoriteMessages.buildFavoriteCycleStatusMessage("empty"),
	favoriteSingle: favoriteMessages.buildFavoriteCycleStatusMessage("single"),
};

write("task34-tips.json", JSON.stringify(tips, null, 2) + "\n");

// Grok chrome: welcome card, tool row and first-time setup render byte-for-byte.
const { GrokWelcomeCard } = await load(source + "grok/welcome-card.ts");
const { GrokToolRow } = await load(source + "grok/tool-row.ts");
const { FirstTimeSetupComponent } = await load(source + "components/first-time-setup.ts");

const renders = {};
for (const width of [40, 60, 80, 120]) {
	renders[`welcome.${width}`] = new GrokWelcomeCard("maho", "1.2.3").render(width);
	renders[`toolRow.partial.${width}`] = new GrokToolRow({ toolName: "bash", isPartial: true }).render(width);
	renders[`toolRow.error.${width}`] = new GrokToolRow({
		toolName: "bash",
		isPartial: false,
		result: { isError: true },
	}).render(width);
	renders[`toolRow.ok.${width}`] = new GrokToolRow({
		toolName: "read",
		isPartial: false,
		result: { isError: false },
	}).render(width);
}
const setup = new FirstTimeSetupComponent({
	detectedTheme: "dark",
	onThemePreview() {},
	onSubmit() {},
	onCancel() {},
});
for (const width of [40, 60, 80]) {
	renders[`firstTimeSetup.theme.${width}`] = setup.render(width);
}
setup.handleInput("\r");
for (const width of [40, 60, 80]) {
	renders[`firstTimeSetup.analytics.${width}`] = setup.render(width);
}

write("task34-render.json", JSON.stringify(renders, null, 2) + "\n");
console.log("Generated pinned task34 ask-user, tips and chrome fixtures");
