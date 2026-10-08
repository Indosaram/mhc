// Shared deterministic loopback provider for the CLI QA scripts: an openai-completions endpoint
// that always streams the same fixed completion, plus the isolated agent dir that points at it.
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

export const REPLY = "offline acceptance";

/** Pre-create the OMO onboarding marker so the onboarding component does not fire a startup turn.
 *  state_dir = `cwd/.maho` (maho-cli omo_mount), marker file `onboarding-completed`
 *  (maho-omo-onboarding/src/state.rs). Applied uniformly by every QA script for a deterministic
 *  idle start (no extra model request, editor idle at ready). */
export function markOnboardingComplete(cwd) {
	mkdirSync(join(cwd, ".maho"), { recursive: true });
	writeFileSync(join(cwd, ".maho", "onboarding-completed"), "");
}

export function startLoopback() {
	const server = Bun.serve({
		hostname: "127.0.0.1", port: 0,
		fetch() {
			const body = [
				`data: ${JSON.stringify({ id: "offline", object: "chat.completion.chunk", created: 0, model: "offline", choices: [{ index: 0, delta: { role: "assistant", content: REPLY }, finish_reason: null }] })}`,
				"",
				`data: ${JSON.stringify({ id: "offline", object: "chat.completion.chunk", created: 0, model: "offline", choices: [{ index: 0, delta: {}, finish_reason: "stop" }] })}`,
				"",
				"data: [DONE]",
				"",
				"",
			].join("\n");
			return new Response(body, { headers: { "content-type": "text/event-stream" } });
		},
	});
	return { server, baseUrl: `http://127.0.0.1:${server.port}/v1` };
}

export function writeOfflineAgent(home, baseUrl) {
	const agent = join(home, "agent");
	mkdirSync(agent, { recursive: true });
	// The mounted immutable-latest composition uses agent-global product state.
	const state = join(agent, "omo-senpi", "omo-native");
	mkdirSync(state, { recursive: true });
	writeFileSync(join(state, "onboarding-completed"), "");
	writeFileSync(join(agent, "models.json"), JSON.stringify({ providers: { offline: {
		api: "openai-completions", baseUrl, apiKey: "offline-fixture",
		models: [{ id: "offline", reasoning: false, input: ["text"], contextWindow: 128000, maxTokens: 4096 }],
	} } }));
	return agent;
}

