#!/usr/bin/env bun
// Installed-binary acceptance probe for eval kernels and terminal monitor.
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, watch, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const SCHEMA = "hg-v-eval/v1";
const EXPECTED_SHA = "b0088a1a4449e3b10d5747d60ecd64c77ea0fbede9538266f98243931e06c50a";
const REPO = "/home/projects/mhc-wt/hg-integration";
const HARNESS = join(REPO, ".omo/evidence/session2-residual/harness-v2");
const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");

function usage(message) {
	console.error(`hg-v-eval: ${message}`);
	console.error("usage: bun tools/hg-v-eval.mjs --binary <mhc> --install <dir> --evidence <dir> --scenario all|eval-js|eval-py|monitor|cleanup");
	process.exit(2);
}
function parseArgs(argv) {
	const args = {};
	for (let i = 0; i < argv.length; i++) {
		const key = argv[i];
  if (["--binary", "--install", "--evidence", "--scenario", "--expected-sha"].includes(key)) args[key.slice(2)] = argv[++i];
		else usage(`unknown argument ${key}`);
	}
	for (const key of ["binary", "install", "evidence"]) if (!args[key]) usage(`--${key} is required`);
	args.scenario ??= "all";
	return args;
}
function fingerprint(path) {
	return existsSync(path) ? { sha256: sha256(readFileSync(path)), mtimeMs: statSync(path).mtimeMs } : null;
}
function newIsolated(prefix) {
	const root = mkdtempSync(join(tmpdir(), prefix));
	const home = join(root, "home");
	const cwd = join(root, "cwd");
	const agent = join(home, "agent");
	for (const path of [home, cwd, agent, join(agent, "omo-senpi", "omo-native")]) mkdirSync(path, { recursive: true });
	writeFileSync(join(agent, "models.json"), JSON.stringify({ providers: {} }));
	return { root, home, cwd, agent, env: { ...process.env, HOME: home, MAHO_CODING_AGENT_DIR: agent, MAHO_MEMORY_HOME: join(home, "memory"), PWD: cwd } };
}
function cleanupReceipt(env, pid = null, sockets = []) {
	let alive = false;
	if (pid) { try { process.kill(pid, 0); alive = true; } catch {} }
	for (const socket of sockets) { try { rmSync(socket, { force: true }); } catch {} }
	const root = env.root;
	rmSync(root, { recursive: true, force: true });
	return { pid, process_gone: !alive, temp_root: root, home: env.home, cwd: env.cwd, agent_dir: env.agent, temp_root_removed: !existsSync(root), sockets, sockets_gone: sockets.every((socket) => !existsSync(socket)), child_pids: [], child_processes_gone: true };
}
function missingSource(path, needle) {
	const lines = readFileSync(path, "utf8").split("\n");
	const index = lines.findIndex((line) => line.includes(needle));
	return `${path}:${index < 0 ? 1 : index + 1}`;
}

function waitForCreatedFile(path, timeoutMs) {
	if (existsSync(path)) return Promise.resolve(Number(readFileSync(path, "utf8").trim()));
	return new Promise((resolvePromise, reject) => {
		const watcher = watch(join(path, ".."), () => {
			if (!existsSync(path)) return;
			watcher.close(); clearTimeout(timer);
			resolvePromise(Number(readFileSync(path, "utf8").trim()));
		});
		const timer = setTimeout(() => { watcher.close(); reject(new Error(`monitor pid file did not appear: ${path}`)); }, timeoutMs);
	});
}

async function scenarioBlocked(name, invocation, contract, artifacts = []) {
	return { status: "blocked", invocation, observable: { pass: false, reason: `unreachable surface; missing contract ${contract}` }, artifacts, cleanup: { pids: [], sockets: [], temp_dirs: [], proof: "No scenario processes were started." } };
}

async function scenarioEval(ctx, language) {
	return modelProbe(ctx, `eval-${language}`, "eval", {language, code: 'import os\nprint("HG_KERNEL_PID=" + str(os.getpid()))\nprint("HG_EVAL_VALUE=" + str(6*7))', summary: "Compute the installed acceptance value", on_timeout: "error", timeout: 20}, "HG_EVAL_VALUE=42");
}

async function scenarioMonitor(ctx) {
	return modelProbe(ctx, "monitor", "eval", {language: "js", code: null}, "HG_MONITOR_SENTINEL");
}

async function modelProbe(ctx, name, toolName, params, expected) {
	if (params.language === "js" && name === "eval-js") params.code = 'print("HG_KERNEL_PID=" + process.pid); print("HG_EVAL_VALUE=" + (6*7))';
	const dirs = newIsolated(`hg-v-${name}-`);
	if (name === "monitor") params = {language:"js",code:`print("HG_KERNEL_PID=" + process.pid); const result = await tool.monitor({description:"Installed sentinel",command:${JSON.stringify(`mkfifo '${join(dirs.root, "monitor-release")}'; IFS= read -r release < '${join(dirs.root, "monitor-release")}'; printf 'HG_MONITOR_PID=%s HG_MONITOR_SENTINEL\\n' "$BASHPID"; IFS= read -r release < '${join(dirs.root, "monitor-release")}'`)},filter:"HG_MONITOR_SENTINEL",timeout_ms:60000}); print("HG_MONITOR_REGISTERED=" + JSON.stringify(result));`,summary:"Start the installed sentinel monitor",on_timeout:"error",timeout:20};
	const requests = [], frames = [];
	let count = 0, proc, socket, stdout, stderr, error = null, kernelExitObservers = [], monitorId = null, monitorPid = null;
	const armingKernels = new Map();
	const armKernels = frame => {
		for (const match of JSON.stringify(frame).matchAll(/HG_KERNEL_PID=(\d+)/g)) {
			const pid = Number(match[1]);
			if (armingKernels.has(pid)) continue;
			armingKernels.set(pid, armPidfd(pid).then(observed => ({ pid, kind: "kernel", ...observed }), armError => ({ pid, kind: "kernel", armed: false, armError: armError.message, observer: null, completed: Promise.resolve("") })));
		}
	};
	let phase = "initial";
	const provider = Bun.serve({hostname:"127.0.0.1",port:0,async fetch(request) {
		if (new URL(request.url).pathname === "/trigger") { count = 0; return new Response("ok"); }
		const body = await request.json();
		requests.push(body);
		if (!body.tools?.some(tool => tool.function?.name === toolName)) {
			return new Response(`data: ${JSON.stringify({id:"hg-facts",object:"chat.completion.chunk",created:0,model:"offline",choices:[{index:0,delta:{content:"No durable facts."},finish_reason:"stop"}]})}\n\ndata: [DONE]\n\n`, {headers:{"content-type":"text/event-stream"}});
		}
		const call = ++count === 1;
		const selectedTool = name === "monitor" && phase === "kill" ? "kill_bash" : toolName;
		const registeredFrame = frames.find(frame => JSON.stringify(frame).includes("HG_MONITOR_REGISTERED={"));
		const registeredText = registeredFrame?.partialResult?.content?.map(part => part.text ?? "").join("\n") ?? "";
		const registeredId = registeredText.match(/"bash_id"\s*:\s*"([^"]+)"/)?.[1];
		const selectedParams = name === "monitor" && phase === "kill" ? {bash_id:registeredId} : params;
		const delta = call ? {tool_calls:[{index:0,id:phase === "kill" ? "hg_kill" : "hg_probe",type:"function",function:{name:selectedTool,arguments:JSON.stringify(selectedParams)}}]} : {content:"HG_PROBE_FINISHED"};
		return new Response(`data: ${JSON.stringify({id:"hg",object:"chat.completion.chunk",created:0,model:"offline",choices:[{index:0,delta,finish_reason:call?"tool_calls":"stop"}]})}\n\ndata: [DONE]\n\n`,{headers:{"content-type":"text/event-stream"}});
	}});
	writeFileSync(join(dirs.agent,"models.json"),JSON.stringify({providers:{offline:{api:"openai-completions",baseUrl:`http://127.0.0.1:${provider.port}/v1`,apiKey:"offline-fixture",models:[{id:"offline",reasoning:false,input:["text"],contextWindow:128000,maxTokens:4096}]}}}));
	writeFileSync(join(dirs.agent,"omo-senpi","omo-native","onboarding-completed"),"");
	const path = join(dirs.root,"host.sock");
	const invocation = [ctx.binary,"--mode","rpc","--multi-session","--listen",`unix://${path}`,"--offline","--provider","offline","--model","offline"];
 let seq = 0, buffer = "", pending = new Map(), finish;
	const finished = new Promise(resolve => { finish = resolve; });
	const frameWaiters = new Set();
	const waitForFrame = (predicate, timeoutMs) => {
		const existing = frames.find(predicate);
		if (existing) return Promise.resolve(existing);
		return new Promise((resolvePromise, reject) => {
			const waiter = { predicate, resolve: resolvePromise };
			frameWaiters.add(waiter);
			const timer = setTimeout(() => { frameWaiters.delete(waiter); reject(new Error("bounded wait for matching RPC frame expired")); }, timeoutMs);
			waiter.resolve = frame => { clearTimeout(timer); frameWaiters.delete(waiter); resolvePromise(frame); };
		});
	};
	const observe = frame => name !== "monitor" && JSON.stringify(frame).includes("HG_PROBE_FINISHED");
	const actualMonitorEvent = frame => {
		if (name !== "monitor") return false;
		const type = frame.type ?? frame.data?.type;
		const payload = type === "send_custom_message" ? frame : frame.message ?? frame.data?.message ?? frame;
		return JSON.stringify(frame).includes("senpi-monitor:notification") && JSON.stringify(frame).includes("HG_MONITOR_SENTINEL") && !JSON.stringify(frame).includes("HG_MONITOR_REGISTERED");
	};
	const deadline = setTimeout(()=>{error = "bounded scenario deadline exceeded"; finish();},45000);
	try {
		const ready = new Promise((resolve,reject)=>{const watcher=watch(dirs.root,()=>{if(existsSync(path)){watcher.close();clearTimeout(timer);resolve();}}); const timer=setTimeout(()=>{watcher.close();reject(new Error("host socket readiness timeout"));},10000);});
		proc = Bun.spawn(invocation,{cwd:dirs.cwd,env:{PATH:process.env.PATH,HOME:dirs.home,MAHO_CODING_AGENT_DIR:dirs.agent,MAHO_MEMORY_HOME:join(dirs.home,"memory"),TMPDIR:dirs.root},stdin:"ignore",stdout:"pipe",stderr:"pipe"});
		stdout = new Response(proc.stdout).text(); stderr = new Response(proc.stderr).text();
		await ready;
		socket = await Bun.connect({unix:path,socket:{data(_socket,chunk){buffer+=chunk.toString();let i;while((i=buffer.indexOf("\n"))>=0){const line=buffer.slice(0,i);buffer=buffer.slice(i+1);const frame=JSON.parse(line);frames.push(frame);armKernels(frame);for(const waiter of [...frameWaiters])if(waiter.predicate(frame))waiter.resolve(frame);if(frame.type==="response"&&pending.has(String(frame.id))){pending.get(String(frame.id))(frame);pending.delete(String(frame.id));}if(observe(frame))finish();}},error(_socket,e){error=e.message;finish();}}});
		const rpc=(type,fields={})=>new Promise(resolve=>{const id=String(++seq);pending.set(id,resolve);socket.write(JSON.stringify({id,type,...fields})+"\n");});
		const opened=await Promise.race([rpc("open_session",{cwd:dirs.cwd}),finished.then(()=>null)]);
		if (!opened?.data?.sessionId) throw new Error("open_session did not return a session id");
		const monitorRegistration = name === "monitor" ? waitForFrame(frame => frame.type === "tool_execution_update" && JSON.stringify(frame).includes("HG_MONITOR_REGISTERED={"), 30000) : null;
		const monitorIdle = name === "monitor" ? waitForFrame(frame => frame.type === "agent_idle", 30000) : null;
		const monitorEventWait = name === "monitor" ? waitForFrame(actualMonitorEvent, 15000) : null;
		monitorEventWait?.catch(() => {});
		await Promise.race([rpc("prompt",{sessionId:opened.data.sessionId,message:`Execute the ${toolName} acceptance probe.`}),finished]);
		if (name === "monitor") {
			const registered = await monitorRegistration;
			const raw = registered.partialResult.content.map(part => part.text ?? "").join("\n");
			const bashId = raw.match(/"bash_id"\s*:\s*"([^"]+)"/)?.[1];
			if (!bashId) throw new Error("monitor registration result did not expose bash_id");
			monitorId = bashId;
			await monitorIdle;
			const reloaded = await rpc("reload", {sessionId:opened.data.sessionId});
			if (!reloaded.success || reloaded.data?.cancelled) throw new Error("monitor reload did not complete");
			const release = Bun.spawn(["/bin/sh", "-c", 'printf "fire\\n" > "$1"', "sh", join(dirs.root,"monitor-release")], {stdout:"ignore",stderr:"pipe"});
			const releaseDeadline = setTimeout(() => release.kill("SIGKILL"), 5000);
			const releaseCode = await release.exited;
			clearTimeout(releaseDeadline);
			if (releaseCode !== 0) throw new Error("monitor trigger FIFO has no surviving reader: " + await new Response(release.stderr).text());
			const monitorEvent = await monitorEventWait;
			monitorPid = Number(JSON.stringify(monitorEvent).match(/HG_MONITOR_PID=(\d+)/)?.[1]);
			if (!monitorPid) throw new Error("monitor process ID was not present in its actual sentinel output frame");
			kernelExitObservers.push({pid:monitorPid,kind:"monitor",...(await armPidfd(monitorPid))});
			monitorId = bashId;
			params = {language:"js",code:`print("HG_KERNEL_PID=" + process.pid); print(await tool.kill_bash({bash_id:${JSON.stringify(bashId)}}))`,summary:"Stop the registered sentinel monitor",on_timeout:"error",timeout:20};
			phase = "kill"; count = 0;
			await fetch(`http://127.0.0.1:${provider.port}/trigger`, {method:"POST"});
			const killPrompt = rpc("prompt", {sessionId:opened.data.sessionId,message:"Stop the acceptance monitor."});
			await killPrompt;
			await waitForFrame(frame => frame.type === "tool_execution_end" && JSON.stringify(frame.result).includes(`Killed ${bashId}.`), 30000);
		} else await finished;
	} catch(e) {error=e.message;} finally {
		clearTimeout(deadline); socket?.end();
		const subscribed = new Set(kernelExitObservers.map(proof => proof.pid));
		kernelExitObservers.push(...(await Promise.all(armingKernels.values())).filter(proof => !subscribed.has(proof.pid)));
		if(proc){proc.kill("SIGTERM");const timer=setTimeout(()=>proc.kill("SIGKILL"),5000);await proc.exited;clearTimeout(timer);}
		provider.stop(true);
	}
	const childExitProofs = await Promise.all(kernelExitObservers.map(async ({pid, kind, observer, armed, armError, completed}) => {
		const fullOutput = await completed;
		if (!observer) return { pid, kind, subscribedBeforeTermination: false, exitObserved: false, output: fullOutput, stderr: armError };
		const exitObserved = armed && await observer.exited === 0 && fullOutput.includes("EXITED");
		return { pid, kind, subscribedBeforeTermination: armed, exitObserved, output: fullOutput, stderr: await new Response(observer.stderr).text() };
	}));
	const out = stdout ? await stdout : "", err = stderr ? await stderr : "";
	const artifact = join(ctx.evidence,`${name}.json`);
	writeFileSync(artifact,JSON.stringify({requests,frames,stdout:out,stderr:err,error},null,2));
	const results=requests.flatMap(r=>(r.messages??[]).filter(m=>m.role==="tool"));
	const monitorEvent = frames.find(actualMonitorEvent) ?? null;
	const monitorExit = frames.find(frame => frame.type === "tool_execution_end" && JSON.stringify(frame.result).includes("Killed bash_1.")) ?? null;
	const pass=!error && childExitProofs.length > 0 && childExitProofs.every(proof => proof.subscribedBeforeTermination && proof.exitObserved) && (name === "monitor"
		? monitorEvent !== null && monitorId !== null && monitorExit !== null
		: results.some(result=>JSON.stringify(result).includes(expected)));
	const cleanup = cleanupReceipt(dirs,proc?.pid,[path]);
	return {status:pass?"pass":"fail",invocation,observable:{pass,error,tool_result_count:results.length,expected,monitor_id:monitorId,monitor_child_pid:monitorPid,monitor_event:monitorEvent,monitor_exit_event:monitorExit ?? null},artifacts:[artifact],cleanup:{...cleanup, childExitProofs, child_pids: childExitProofs.map(proof => proof.pid), child_processes_gone: childExitProofs.length > 0 && childExitProofs.every(proof => proof.subscribedBeforeTermination && proof.exitObserved)}};
}

async function armPidfd(pid) {
	const observer = Bun.spawn(["python3", "-u", "-c", "import os,select,sys\nfd=os.pidfd_open(int(sys.argv[1])); print('ARMED',flush=True); p=select.poll(); p.register(fd,select.POLLIN); print('EXITED' if p.poll(15000) else 'TIMEOUT',flush=True)", String(pid)], { stdout: "pipe", stderr: "pipe" });
	const reader = observer.stdout.getReader();
	const decoder = new TextDecoder();
	let output = "";
	const armed = await new Promise((resolve, reject) => {
		const timer = setTimeout(() => reject(new Error(`pidfd subscription timeout for kernel ${pid}`)), 3000);
		(async () => { for (;;) { const { value, done } = await reader.read(); if (done) { clearTimeout(timer); reject(new Error(`pidfd subscription failed for ${pid}: ${await new Response(observer.stderr).text()}`)); return; } output += decoder.decode(value, { stream: true }); if (output.includes("ARMED\n")) { clearTimeout(timer); resolve(true); return; } } })().catch(reject);
	});
	const completed = (async () => { for (;;) { const { value, done } = await reader.read(); if (done) break; output += decoder.decode(value, { stream: true }); } return output; })();
	return { observer, armed, get output() { return output; }, completed };
}

async function scenarioCleanup(scenarios) {
	const proofs = Object.values(scenarios).flatMap(scenario => scenario.cleanup?.childExitProofs ?? []);
	const monitor = scenarios.monitor;
	const monitorProof = monitor?.cleanup?.childExitProofs?.find(proof => proof.kind === "monitor");
	const pass = proofs.length >= 4 && proofs.every(proof => typeof proof.pid === "number" && proof.subscribedBeforeTermination === true && proof.exitObserved === true)
		&& typeof monitor?.observable?.monitor_id === "string" && typeof monitor?.observable?.monitor_child_pid === "number"
		&& monitor?.observable?.monitor_event?.entry?.customType === "senpi-monitor:notification"
		&& monitor?.observable?.monitor_event?.entry?.content?.some(part => part.text.includes("HG_MONITOR_SENTINEL"))
		&& monitorProof?.pid === monitor.observable.monitor_child_pid;
	return { status: pass ? "pass" : "blocked", invocation: "Kernel and monitor pidfd exit observations from preceding scenarios", observable: { pass }, artifacts: [], cleanup: { pids: proofs.map(proof => proof.pid), childExitProofs: proofs, proof: "Bounded pidfd subscriptions armed before host termination." } };
}

async function main() {
	const args = parseArgs(process.argv.slice(2));
	const binary = resolve(args.binary);
	const install = resolve(args.install);
	const evidence = resolve(args.evidence);
	const qaRoot = join(evidence, "qa");
	mkdirSync(qaRoot, { recursive: true });
	const before = fingerprint(binary);
	const names = args.scenario === "all" ? ["eval-js", "eval-py", "monitor", "cleanup"] : [args.scenario];
	const allowed = new Set(["eval-js", "eval-py", "monitor", "cleanup"]);
	for (const name of names) if (!allowed.has(name)) usage(`unknown scenario ${name}`);
	const scenarios = {};
	for (const name of names) {
		if (name === "eval-js" || name === "eval-py") scenarios[name] = await scenarioEval({ binary, install, evidence }, name.slice(5));
		else if (name === "monitor") scenarios[name] = await scenarioMonitor({ binary, install, evidence });
		else scenarios[name] = await scenarioCleanup(scenarios);
	}
	const after = fingerprint(binary);
 const binaryStable = JSON.stringify(before) === JSON.stringify(after) && before?.sha256 === (args["expected-sha"] ?? EXPECTED_SHA);
	const report = { schema: SCHEMA, binary, binary_before: before, binary_after: after, binary_stable: binaryStable, scenarios, status: binaryStable && Object.values(scenarios).every((item) => item.status === "pass") ? "pass" : "blocked" };
	writeFileSync(join(qaRoot, "hg-v-eval.json"), JSON.stringify(report, null, 2) + "\n");
	console.log(JSON.stringify(report, null, 2));
	process.exitCode = report.status === "pass" ? 0 : 1;
}

await main();
