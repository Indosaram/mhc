#!/usr/bin/env bun
// Interactive faux reference: registers senpi's faux provider with a scripted turn, then runs
// senpi's own interactive mode (packages/coding-agent/src/main.ts) in the current terminal with
// `--provider faux --model faux-1`. `--omo` also loads omo-senpi's extension, giving the omo UI.
//
//   bun tools/golden/faux-tui.mjs --script hello [--omo] [-- <extra senpi args>]
//
// This is the reference side of pty comparisons, e.g.
//   bun /Users/indo/code/oh-my-openagent/script/qa/web-terminal-visual-qa.mjs --title ref \
//     --command "bun tools/golden/faux-tui.mjs --script hello" --input "Say hello." --input "{Enter}" \
//     --cols 100 --rows 30 --evidence-dir <dir>
// It runs under a private temp HOME in offline mode, so the real ~/.senpi and ~/.omo are never
// read or written and no update check or tool download happens.
import { isolateHome, loadScript, setupFaux } from "./faux-common.mjs";

function usage(message) {
	console.error(`faux-tui: ${message}\nusage: bun tools/golden/faux-tui.mjs --script <name> [--omo] [-- <senpi args>]`);
	process.exit(2);
}

let name;
let omo = false;
let extra = [];
const argv = process.argv.slice(2);
for (let i = 0; i < argv.length; i++) {
	if (argv[i] === "--script") name = argv[++i];
	else if (argv[i] === "--omo") omo = true;
	else if (argv[i] === "--") {
		extra = argv.slice(i + 1);
		break;
	} else usage(`unknown argument ${argv[i]}`);
}
const script = loadScript(name, usage);

isolateHome();
const faux = await setupFaux(script, { omo });
const { main } = await faux.load("packages/coding-agent/src/main.ts");
await main(["--offline", "--provider", faux.model.provider, "--model", faux.model.id, ...extra], {
	extensionFactories: [faux.fauxExtension, ...faux.extensions],
});
