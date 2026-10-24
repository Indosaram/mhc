#!/usr/bin/env bun
// Static release-asset consistency check (no build, no network).
//  1. every release.yml build-matrix target uploads archive, .sha256 and .minisig (upload-artifact globs)
//  2. all five supported targets are in the matrix
//  3. the pinned public key literal in install.sh / install.ps1 equals mhc-release.pub
// Usage: bun tools/qa/release-assets-check.mjs [--root <repo dir>]
import { existsSync, readFileSync } from "node:fs";
import { resolve } from "node:path";

const REQUIRED_TARGETS = ["linux-x86_64", "linux-aarch64", "macos-aarch64", "macos-x86_64", "windows-x86_64"];
const argv = process.argv.slice(2);
let root = resolve(import.meta.dir, "..", "..");
for (let i = 0; i < argv.length; i++) {
	if (argv[i] === "--root" && i + 1 < argv.length) root = resolve(argv[++i]);
	else throw new Error(`Unknown or incomplete argument: ${argv[i]}`);
}

const errors = [];
const fail = message => errors.push(message);

function globToRegExp(glob) {
	const escaped = glob.replace(/[.+^${}()|[\]\\]/g, "\\$&");
	return new RegExp(`^${escaped.replace(/\*\*/g, "\u0000").replace(/\*/g, "[^/]*").replace(/\u0000/g, ".*")}$`);
}

function read(rel) {
	const full = resolve(root, rel);
	if (!existsSync(full)) {
		fail(`${rel} is missing`);
		return null;
	}
	return readFileSync(full, "utf8");
}

function assetNames(target) {
	const ext = target.startsWith("windows") ? "zip" : "tar.gz";
	const base = `mhc-${target}.${ext}`;
	return [base, `${base}.sha256`, `${base}.minisig`];
}

const releaseText = read(".github/workflows/release.yml");
if (releaseText) {
	const workflow = Bun.YAML.parse(releaseText);
	const build = workflow?.jobs?.build;
	const matrixTargets = (build?.strategy?.matrix?.include ?? []).map(row => row.target);
	for (const target of REQUIRED_TARGETS) {
		if (!matrixTargets.includes(target)) fail(`release.yml build matrix lacks target ${target}`);
	}
	const upload = (build?.steps ?? []).find(step => String(step.uses ?? "").startsWith("actions/upload-artifact"));
	if (!upload) {
		fail("release.yml build job has no upload-artifact step");
	} else {
		const globs = String(upload.with?.path ?? "").split("\n").map(line => line.trim()).filter(Boolean).map(globToRegExp);
		for (const target of matrixTargets) {
			for (const name of assetNames(target)) {
				for (const dir of ["dist", "dist/candidate-A", "dist/candidate-B"]) {
					if (!globs.some(re => re.test(`${dir}/${name}`))) fail(`release.yml upload-artifact path does not cover ${dir}/${name}`);
				}
			}
		}
	}
}

const pub = read("mhc-release.pub");
const pubKey = pub?.split("\n").map(line => line.trim()).find(line => /^RW[A-Za-z0-9+/=]+$/.test(line));
if (pub && !pubKey) fail("mhc-release.pub has no minisign public key line");
for (const [rel, re] of [
	["install.sh", /PUBKEY\w*=['"](RW[A-Za-z0-9+/=]+)['"]/i],
	["install.ps1", /PUBKEY\w*\s*=\s*['"](RW[A-Za-z0-9+/=]+)['"]/i],
]) {
	const text = read(rel);
	if (!text) continue;
	const literal = re.exec(text)?.[1];
	if (!literal) fail(`${rel} has no pinned public key literal`);
	else if (pubKey && literal !== pubKey) fail(`${rel} public key literal does not equal mhc-release.pub`);
}

if (errors.length > 0) {
	console.error(`release-assets-check FAILED (${errors.length}):\n- ${errors.join("\n- ")}`);
	process.exit(1);
}
console.log("release-assets-check ok");
