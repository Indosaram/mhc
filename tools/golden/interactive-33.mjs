import { mkdirSync, writeFileSync } from "node:fs";
import { resolve, join } from "node:path";
import { pathToFileURL } from "node:url";
import { pinnedSenpiRoot } from "./pin.mjs";

const root = pinnedSenpiRoot();
const source = "packages/coding-agent/src/modes/interactive/";
const load = (path) => import(pathToFileURL(join(root, path)).href);

const { rankModelSearchItems } = await load(source + "model-search-rank.ts");
const { getModelSearchText } = await load(source + "model-search.ts");
const search = await load(source + "components/session-selector-search.ts");
const favorites = await load(source + "components/model-favorites.ts");

const models = [
	{ provider: "anthropic", id: "opus-4-5", name: "the model 4.5" },
	{ provider: "anthropic", id: "opus-4-6", name: "the model 4.6" },
	{ provider: "openai", id: "gpt-5.2", name: "GPT-5.2" },
	{ provider: "openai", id: "gpt-5.6-sol", name: "GPT-5.6 Sol" },
	{ provider: "openrouter", id: "anthropic/opus-4-5", name: "Anthropic: the model 4.5" },
	{ provider: "google", id: "gemini-3-pro-preview", name: "Gemini 3 Pro Preview" },
	{ provider: "zai", id: "glm-5.2", name: "GLM-5.2" },
	{ provider: "groq", id: "moonshotai/kimi-k3", name: "Kimi K3" },
];
const favoriteIds = ["openai/gpt-5.2"];
const currentModel = undefined;
const queries = ["opus", "gpt", "the model", "", "openrouter", "kimi", "anthropic opus"];

// senpi's ModelSelectorComponent sorts its snapshot before ranking: current first, then
// favorites, then provider/id (sortModels).
const sortedModels = [...models].sort((a, b) => {
	const aCurrent = currentModel && a.id === currentModel.id && a.provider === currentModel.provider;
	const bCurrent = currentModel && b.id === currentModel.id && b.provider === currentModel.provider;
	if (aCurrent && !bCurrent) return -1;
	if (!aCurrent && bCurrent) return 1;
	const aFavorite = favoriteIds.includes(a.provider + "/" + a.id);
	const bFavorite = favoriteIds.includes(b.provider + "/" + b.id);
	if (aFavorite && !bFavorite) return -1;
	if (!aFavorite && bFavorite) return 1;
	const providerCompare = a.provider.localeCompare(b.provider);
	if (providerCompare !== 0) return providerCompare;
	return a.id.localeCompare(b.id);
});

const modelSelector = queries.map((query) => ({
	query,
	rows: rankModelSearchItems(sortedModels, query, (item) => item, {
		favoritesFirst: true,
		isFavorite: (item) => favoriteIds.includes(item.provider + "/" + item.id),
	}).map((item) => item.provider + "/" + item.id),
}));
const searchText = models.map(getModelSearchText);

const sessions = [
	{ id: "s1", name: "alpha node cve", path: "/tmp/a.jsonl", cwd: "/work", firstMessage: "hello", allMessagesText: "hello world node cve fix", modified: new Date(1_700_000_300_000), messageCount: 3 },
	{ id: "s2", path: "/tmp/b.jsonl", cwd: "/work", firstMessage: "other", allMessagesText: "unrelated text", modified: new Date(1_700_000_200_000), messageCount: 1 },
	{ id: "s3", name: "beta", path: "/tmp/c.jsonl", cwd: "/other", firstMessage: "beta", allMessagesText: "beta body", modified: new Date(1_700_000_100_000), messageCount: 2 },
];
const sessionQueries = ['node cve', '"node cve"', "re:beta", "re:[", "beta", ""];
const sessionSearch = sessionQueries.map((query) => ({
	query,
	parsedError: search.parseSearchQuery(query).error ?? null,
	recent: search.filterAndSortSessions(sessions, query, "recent").map((session) => session.id),
	relevance: search.filterAndSortSessions(sessions, query, "relevance").map((session) => session.id),
	named: search.filterAndSortSessions(sessions, query, "recent", "named").map((session) => session.id),
}));

const allIds = models.map((item) => item.provider + "/" + item.id);
const favoritesGolden = {
	toggleFromAll: favorites.toggleFavoriteModel(null, allIds, "openai/gpt-5.2"),
	toggleOff: favorites.toggleFavoriteModel(favoriteIds, allIds, "openai/gpt-5.2"),
	toggleOn: favorites.toggleFavoriteModel([], allIds, "zai/glm-5.2"),
	favoriteAll: favorites.favoriteModels([], allIds),
	clearFiltered: favorites.clearFavoriteModels(favoriteIds, allIds, ["openai/gpt-5.2"]),
	clearAll: favorites.clearFavoriteModels(null, allIds),
	moveUp: favorites.moveFavoriteModel(["a", "b", "c"], "b", -1),
	sorted: favorites.getSortedFavoriteModelIds(["zai/glm-5.2"], allIds),
};

const dir = resolve(import.meta.dir, "../../crates/maho-interactive/tests/golden");
mkdirSync(dir, { recursive: true });
const sessionFixture = sessions.map((session) => ({
	id: session.id,
	name: session.name ?? null,
	path: session.path,
	cwd: session.cwd,
	firstMessage: session.firstMessage,
	allMessagesText: session.allMessagesText,
	modifiedMs: session.modified.getTime(),
	messageCount: session.messageCount,
}));
const out = {
	models,
	favoriteIds,
	modelSelector,
	searchText,
	sessions: sessionFixture,
	sessionSearch,
	favorites: favoritesGolden,
};
writeFileSync(join(dir, "task33-selectors.json"), JSON.stringify(out, null, 2) + "\n");
console.log("wrote task33-selectors.json");

// Exported helper functions of the todo-33 modules, recorded verbatim.
const { formatKeyText } = await load(source + "components/keybinding-hints.ts");
const { formatAuthSelectorProviderType } = await load(source + "components/oauth-selector.ts");
const http = await load("packages/coding-agent/src/core/http-dispatcher.ts");
const helpers = {
	formatKeyText: ["escape", "Escape", "ctrl+a/escape", "shift+ctrl+p", "alt+up", "pageUp"].map((key) => [
		key,
		formatKeyText(key),
		formatKeyText(key, { capitalize: true }),
	]),
	formatAuthSelectorProviderType: ["oauth", "api_key"].map((authType) => [
		authType,
		formatAuthSelectorProviderType(authType),
	]),
	hasSessionName: [sessions[0], sessions[1], { ...sessions[0], name: "   " }].map((session) => [
		session.id,
		search.hasSessionName(session),
	]),
	formatHttpIdleTimeoutMs: [0, 30_000, 45_000, 300_000].map((ms) => [ms, http.formatHttpIdleTimeoutMs(ms)]),
};
const withHelpers = { ...out, helpers };
writeFileSync(join(dir, "task33-selectors.json"), JSON.stringify(withHelpers, null, 2) + String.fromCharCode(10));
console.log("wrote task33-selectors.json with helpers");
