#!/usr/bin/env bun
// Installer signature QA (no cargo, no network beyond loopback).
//   bun tools/qa/install-verify.mjs                          run the install.sh (Linux/macOS) or install.ps1 (Windows, via pwsh) scenarios
//   bun tools/qa/install-verify.mjs --emit-test-installers D write D/install.sh and D/install.ps1 with the pinned key
//                                                            replaced by the fixture key release-test-key/test.pub
// Scenarios use a throwaway minisign key pair generated here; the installer copy under test gets that key substituted.
import { chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const root = resolve(import.meta.dir, "..", "..");
const isWindows = process.platform === "win32";
const KEY_RE_SH = /(MHC_RELEASE_PUBKEY=')RW[A-Za-z0-9+/=]+(')/;
const KEY_RE_PS = /(\$MhcReleasePubkey\s*=\s*')RW[A-Za-z0-9+/=]+(')/;

function pubKeyOf(path) {
	const key = readFileSync(path, "utf8").split("\n").map(l => l.trim()).find(l => /^RW[A-Za-z0-9+/=]+$/.test(l));
	if (!key) throw new Error(`no public key in ${path}`);
	return key;
}

function withKey(file, key) {
	const text = readFileSync(join(root, file), "utf8");
	const re = file.endsWith(".ps1") ? KEY_RE_PS : KEY_RE_SH;
	if (!re.test(text)) throw new Error(`${file} has no key literal`);
	return text.replace(re, `$1${key}$2`);
}

function emit(dir) {
	const key = pubKeyOf(join(root, "crates/maho-cli/tests/fixtures/release-test-key/test.pub"));
	mkdirSync(dir, { recursive: true });
	for (const file of ["install.sh", "install.ps1"]) writeFileSync(join(dir, file), withKey(file, key));
	chmodSync(join(dir, "install.sh"), 0o755);
	console.log(`wrote test installers to ${dir}`);
}

const args = process.argv.slice(2);
if (args[0] === "--emit-test-installers") {
	if (!args[1]) throw new Error("--emit-test-installers needs a directory");
	emit(resolve(args[1]));
	process.exit(0);
}
if (args.length > 0) throw new Error(`Unknown argument: ${args[0]}`);

const have = cmd => Bun.which(cmd) !== null;
if (!have("minisign")) {
	console.error("install-verify: minisign is required to run this harness");
	process.exit(1);
}
if (isWindows && !have("pwsh")) {
	console.error("install-verify: pwsh is required on Windows");
	process.exit(1);
}

const tmp = mkdtempSync(join(tmpdir(), "mhc-install-verify-"));
const failures = [];
const log = [];
const record = (name, ok, detail = "") => {
	log.push(`${ok ? "PASS" : "FAIL"} ${name}${detail ? `: ${detail}` : ""}`);
	console.log(log.at(-1));
	if (!ok) failures.push(name);
};

async function run(cmd, opts = {}) {
	const proc = Bun.spawn(cmd, { stdout: "pipe", stderr: "pipe", stdin: "ignore", ...opts });
	const [stdout, stderr, code] = await Promise.all([new Response(proc.stdout).text(), new Response(proc.stderr).text(), proc.exited]);
	return { stdout, stderr, code };
}

async function genKey(name) {
	const sec = join(tmp, `${name}.key`);
	const pub = join(tmp, `${name}.pub`);
	const r = await run(["minisign", "-G", "-f", "-W", "-s", sec, "-p", pub]);
	if (r.code !== 0) throw new Error(`minisign -G failed: ${r.stderr}`);
	return { sec, pub: pubKeyOf(pub), pubFile: pub };
}

const trusted = await genKey("trusted");
const attacker = await genKey("attacker");

const target = isWindows ? "windows-x86_64" : `${process.platform === "darwin" ? "macos" : "linux"}-${process.arch === "arm64" ? "aarch64" : "x86_64"}`;
const asset = isWindows ? `mhc-${target}.zip` : `mhc-${target}.tar.gz`;

// Build one archive per fake release version; the payload prints its own version.
async function buildArchive(version) {
	const stage = join(tmp, `stage-${version}`);
	const pkg = join(stage, `mhc-${target}`);
	mkdirSync(pkg, { recursive: true });
	if (isWindows) {
		const rs = join(tmp, `fake-${version}.rs`);
		const exe = join(tmp, `fake-${version}.exe`);
		if (!existsSync(exe)) {
			writeFileSync(rs, `fn main() { println!("${version}"); }\n`);
			const cr = await run(["rustc", "--crate-name", "fake_payload", "-C", "opt-level=0", rs, "-o", exe]);
			if (cr.code !== 0) throw new Error(`rustc failed: ${cr.stderr}`);
		}
		copyFileSync(exe, join(pkg, "mhc.exe"));
		copyFileSync(exe, join(pkg, "mhc-launcher.exe"));
	} else {
		for (const name of ["mhc", "mhc-launcher"]) {
			writeFileSync(join(pkg, name), `#!/bin/sh\necho "mhc ${version}" | sed 's/^mhc //'\n`);
			chmodSync(join(pkg, name), 0o755);
		}
	}
	writeFileSync(join(pkg, "manifest.json"), `{"version":"${version}"}\n`);
	const out = join(tmp, `${version}-${asset}`);
	const r = isWindows ? await run(["tar", "-a", "-cf", out, "-C", stage, `mhc-${target}`]) : await run(["tar", "-czf", out, "-C", stage, `mhc-${target}`]);
	if (r.code !== 0) throw new Error(`archive build failed: ${r.stderr}`);
	return readFileSync(out);
}

async function sign(bytes, key) {
	const file = join(tmp, `sign-${Math.random().toString(36).slice(2)}`);
	writeFileSync(file, bytes);
	const r = await run(["minisign", "-S", "-s", key.sec, "-m", file]);
	if (r.code !== 0) throw new Error(`minisign -S failed: ${r.stderr}`);
	return readFileSync(`${file}.minisig`);
}

const sha = bytes => `${new Bun.CryptoHasher("sha256").update(bytes).digest("hex")}  ${asset}\n`;

let files = new Map();
const server = Bun.serve({
	port: 0,
	fetch(req) {
		const body = files.get(new URL(req.url).pathname.slice(1));
		return body === undefined ? new Response("not found", { status: 404 }) : new Response(body);
	},
});
const baseUrl = `http://127.0.0.1:${server.port}`;

// Script copies with the throwaway trusted key substituted.
const scriptDir = join(tmp, "scripts");
mkdirSync(scriptDir);
writeFileSync(join(scriptDir, "install.sh"), withKey("install.sh", trusted.pub));
writeFileSync(join(scriptDir, "install.ps1"), withKey("install.ps1", trusted.pub));

// PATH without minisign: symlink just the tools install.sh needs.
const noMinisignBin = join(tmp, "nomini-bin");
mkdirSync(noMinisignBin);
if (!isWindows) {
	for (const tool of ["sh", "uname", "mktemp", "curl", "tar", "sha256sum", "shasum", "cut", "tr", "head", "sed", "grep", "cat", "mkdir", "rm", "cp", "mv", "ln", "cmp", "basename", "dirname", "flock", "chmod", "gzip"]) {
		const path = Bun.which(tool);
		if (path) symlinkSync(path, join(noMinisignBin, tool));
	}
}

const SEED_POINTER = `{"schemaVersion":1,"current":"0.9.0","previous":null}\n`;

async function scenario(name, { version, archive = "valid", sig = "trusted", hostedPub = false, allowUnsigned = false, noMinisign = false, expect, message }) {
	const dir = join(tmp, `case-${log.length}`);
	const installDir = join(dir, "root");
	const binDir = join(dir, "bin");
	mkdirSync(installDir, { recursive: true });
	writeFileSync(join(installDir, "current.json"), SEED_POINTER);
	const good = await buildArchive(version.replace(/^v/, ""));
	const served = archive === "tampered" ? Buffer.concat([good, Buffer.from("tampered")]) : good;
	files = new Map([[asset, served], [`${asset}.sha256`, sha(served)]]);
	if (sig === "trusted") files.set(`${asset}.minisig`, await sign(good, trusted));
	else if (sig === "attacker") files.set(`${asset}.minisig`, await sign(served, attacker));
	if (hostedPub) files.set("mhc-release.pub", readFileSync(attacker.pubFile));
	const env = {
		...process.env,
		MHC_VERSION: version,
		MHC_INSTALL_DIR: installDir,
		MHC_BIN_DIR: binDir,
		MHC_INSTALL_BASE_URL: baseUrl,
		MHC_NO_PATH_UPDATE: "1",
		LOCALAPPDATA: dir,
	};
	delete env.MHC_PACKAGE_DIR;
	delete env.MHC_ALLOW_UNSIGNED;
	delete env.MHC_VERIFY_ATTESTATION;
	if (allowUnsigned) env.MHC_ALLOW_UNSIGNED = "1";
	let cmd;
	if (isWindows) {
		cmd = ["pwsh", "-NoProfile", "-File", join(scriptDir, "install.ps1")];
		if (noMinisign) {
			for (const key of ["PATH", "Path"]) {
				if (env[key]) {
					env[key] = env[key]
						.split(";")
						.filter(p => !existsSync(join(p, "minisign.exe")))
						.join(";");
				}
			}
		}
	} else {
		cmd = ["sh", join(scriptDir, "install.sh")];
		if (noMinisign) env.PATH = noMinisignBin;
	}
	const r = await run(cmd, { env });
	const pointer = readFileSync(join(installDir, "current.json"), "utf8");
	const out = `${r.stdout}\n${r.stderr}`;
	let ok;
	let detail = `exit ${r.code}`;
	if (expect === "install") {
		ok = r.code === 0 && pointer.includes(`"current":"${version.replace(/^v/, "")}"`);
		if (ok && message && !out.includes(message)) {
			ok = false;
			detail += `; missing output "${message}"`;
		}
	} else {
		ok = r.code !== 0 && pointer === SEED_POINTER && !existsSync(join(installDir, "versions", version.replace(/^v/, "")));
		if (ok && message && !out.includes(message)) {
			ok = false;
			detail += `; missing output "${message}"`;
		}
	}
	if (!ok) detail += `\n${out.trim()}`;
	record(name, ok, detail);
}

const SIGNED = "v2026.10.24.1";
const OLD = "v2026.10.19";
try {
	await scenario("valid signature installs", { version: SIGNED, expect: "install", message: "Signature verified" });
	await scenario("valid signature installs without minisign (skip notice)", { version: SIGNED, expect: "install", noMinisign: true, message: "skipped signature check" });
	await scenario("tampered archive aborts", { version: SIGNED, archive: "tampered", expect: "abort" });
	await scenario("attacker signature aborts", { version: SIGNED, sig: "attacker", expect: "abort", message: "signature verification failed" });
	await scenario("attacker signature with replaced hosted mhc-release.pub aborts", { version: SIGNED, sig: "attacker", hostedPub: true, expect: "abort", message: "signature verification failed" });
	await scenario("missing signature at first signed version aborts", { version: SIGNED, sig: "none", expect: "abort", message: "no signature" });
	await scenario("missing signature at first signed version aborts even with allow-unsigned", { version: SIGNED, sig: "none", allowUnsigned: true, expect: "abort", message: "no signature" });
	await scenario("missing signature before first signed version aborts by default", { version: OLD, sig: "none", expect: "abort", message: "unsigned" });
	await scenario("missing signature before first signed version installs with allow-unsigned", { version: OLD, sig: "none", allowUnsigned: true, expect: "install" });
} finally {
	server.stop(true);
	rmSync(tmp, { recursive: true, force: true });
}

if (failures.length > 0) {
	console.error(`install-verify FAILED (${failures.length}): ${failures.join("; ")}`);
	process.exit(1);
}
console.log("install-verify ok");
