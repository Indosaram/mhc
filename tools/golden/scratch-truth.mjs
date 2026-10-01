// Scratch: read senpi's real behavior for the cases the Rust tests disagree on.
// Deleted before commit; not part of the deliverable.
import { createInvokeRecoveryStreamParser } from "/home/indo/code/senpi/packages/ai/src/tool-call-middleware/protocols/anthropic-xml/recovery-stream.ts";
import { createAnthropicXmlStreamParser } from "/home/indo/code/senpi/packages/ai/src/tool-call-middleware/protocols/anthropic-xml/stream.ts";
import { parseAnthropicXmlGeneratedText } from "/home/indo/code/senpi/packages/ai/src/tool-call-middleware/protocols/anthropic-xml/parse.ts";
import { anthropicXmlInvokeConfig } from "/home/indo/code/senpi/packages/ai/src/tool-call-middleware/protocols/anthropic-xml/invoke-protocol.ts";
import { createAntmlInvokeRecoveryStreamParser } from "/home/indo/code/senpi/packages/ai/src/tool-call-middleware/protocols/antml/recovery-stream.ts";
import { createXtmlRecoveryStreamParser } from "/home/indo/code/senpi/packages/ai/src/tool-call-middleware/protocols/kimi-xtml/recovery-stream.ts";
import { recoverKimiXtmlThinking } from "/home/indo/code/senpi/packages/ai/src/tool-call-middleware/protocols/kimi-xtml/thinking-recovery.ts";

const weather = {
	name: "get_weather",
	description: "weather",
	parameters: {
		type: "object",
		properties: { city: { type: "string" } },
		required: ["city"],
		additionalProperties: false,
	},
};

const out = (label, value) => console.log(`### ${label}\n${JSON.stringify(value)}\n`);

{
	const p = createInvokeRecoveryStreamParser([weather], anthropicXmlInvokeConfig);
	out("anthropic-recovery feed('hello world')", [...p.feed("hello world"), ...p.finish()]);
}
{
	const p = createAnthropicXmlStreamParser([weather]);
	const feed = p.feed("<div");
	out("anthropic-stream feed('<div')", feed);
	out("anthropic-stream finish()", p.finish());
}
{
	const p = createAntmlInvokeRecoveryStreamParser([weather]);
	out("antml-recovery feed('banana')", [...p.feed("banana"), ...p.finish()]);
}
{
	const p = createXtmlRecoveryStreamParser([weather]);
	p.feed("partial text");
	out("kimi-recovery interrupt()", p.interrupt());
}
{
	out("kimi-thinking 'reasoning<|open|>response<|sep|>the answer'", recoverKimiXtmlThinking({
		role: "assistant",
		api: "openai-completions",
		provider: "p",
		model: "kimi-k3",
		content: [{ type: "thinking", thinking: "reasoning<|open|>response<|sep|>the answer" }],
		usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
		stopReason: "stop",
		timestamp: 1,
	}));
}
{
	const text = '<invoke name="get_weather"><parameter name="unknown_param">x</parameter></invoke>';
	out("parse unknown param (no opts)", parseAnthropicXmlGeneratedText(text, [weather]));
	const seen = [];
	out("parse unknown param (opts)", parseAnthropicXmlGeneratedText(text, [weather], { onError: (m) => seen.push(m) }));
	out("parse unknown param onError messages", seen);
}
