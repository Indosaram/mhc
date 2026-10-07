#!/usr/bin/env bun
// Generates the source-derived matcher corpus for both native rules crates from the pinned
// picomatch parser (`bash: true, dot: true`). Fixtures are produced ONLY here, never hand-written
// or auto-accepted; the barrier in latest-omo-rules-pin.mjs runs first and the manifest records
// the exact source hashes the corpus was derived from.
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { pinnedPicomatch } from "./latest-omo-rules-pin.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");
const CRATES = ["crates/builtins/maho-ext-rules", "crates/extensions/maho-ext-pi-rules"];
const OPTIONS = { bash: true, dot: true };

const patterns = [
	"@(foo|bar).ts", "+(foo|bar).ts", "?(foo|bar).ts", "*(foo|bar).ts", "a@(b|c)d", "@(a|+(b|c))",
	"src/*(foo|bar).ts", "a?(b)c", "a+(b)c", "@(a|b){1..3}", "x/!(foo|bar)", "x/!(*.d).ts", "a!(b)c",
	"@(a|b){x,y}", "@(a|b)?", "@(a|b)+", "+(*(a)|*(b))", "+(*(a)|b)", "+(*(ab)|b)", "+(a|+(b|c))",
	"{1..3}", "{3..1}", "{a..c}", "{a,b}", "a{b,c}", "{a,{b,c}}", "{src,{test,spec}}/*.ts",
	"a{b", "a{b,c", "a{b}", "a}b", "{10..12}", "{z..a}", "foo{,.txt}", "@(a|b){x}",
	'"*"', 'a"*"b', '@(a|b)"*"', '@(a|b)"?"', '"a*b"',
	"@(a|b)[[:digit:]]", "[[:alpha:]]", "[[:digit:]]", "[[:space:]]", "[[:bogus:]]", "[[:punct:]]",
	"@(a|b)[^[:digit:]]", "@(a|b)[abc]", "@(a|b)[!x]", "@(a|b)[^x]",
	"[abc", "[]", "[^]", "[z-a]", "[!x]", "[abc]", "@(a|b)[a-]", "@(a|b)[[]", "@(a|b)[a/b]",
	"@(a|b)[]x]", "@(a|b)[^]x]", "@(a|b)[a&&b]", "@(a|b)[a~b]", "@(a|b)[a||b]", "@(a|b)[a~~b]",
	"*", "**", "**/a", "a/*", "a/**", "a/**/b", "a*/b", "a**/b", "*/a", "a/**/", "a/**/**/b", "./a",
	"*.*", "*/*", "**/*", "**/*.*", "**/.*", ".*",
	"a?b", "a.b", "a*b", "*.rs", "*.md", "foo.ts", "CONTEXT.md",
	"a(b", "a)b", "a|b", "a+b", "(?:a|b)", "(?=a)a", "(ab)+", "a{b", "a(b", "a|@(b|c)", "@(a|b)|c",
	"?", "??", "a?", "a??", "@(a|b)?\ud83d\ude00", "@(a|b)\ud83d\ude00", "[\ud83d\ude00]*", "*\ud83d\ude00",
	// Astral branches inside `+(...)` / `*(...)` bodies: the pinned analyzer measures
	// branches with `String#length` (UTF-16 code units), so these must not collapse to a
	// flat single-unit `[...]*` class. The literal forms are also emitted as paths below.
	"+(*(a)|\ud83d\ude00)", "+(*(\ud83d\ude00))", "+(\ud83d\ude00|\ud83d\ude00)", "+(\ud83d\ude00|\ud83d\ude01)",
	"+(a|\ud83d\ude00)", "@(a|\ud83d\ude00)", "?(\ud83d\ude00|a)", "*(\ud83d\ude00|a)", "+( *(a) | \ud83d\ude00 )",
	"a\ud83d\ude00?", "@(a|b)[\ud83d\ude00]", "+(*(a)|\ud83d\ude00)b", "+(*(\ud83d\ude00)|a)",
	"+( *(a) | b )", '+(a | aa)', '+("a"|b)', "a{1..3}", "@(a|b)[[:bogus:]]", "@(a|b)(?=c)c",
	"src/*.ts", "src/**/*.ts", "src/**", "test/**", "**/*.test.ts", "**/foo.ts",
	// `expandRange` sorts its arguments with `Array#sort` (UTF-16 code-unit order) and the
	// resulting class is emitted in the mapped unit domain, so astral endpoints are
	// exercised through the consumer here.
	"{\ud83d\ude00..\u{e000}}", "{\u{e000}..\ud83d\ude00}", "{\ud83d\ude00..a}", "{a..\ud83d\ude00}",
	"{\ud83d\ude00..\ud83d\ude01}", "{\u{e000}..\u{e001}}",
];
const paths = [
	"foo.ts", "bar.ts", ".ts", "foobar.ts", "foofoo.ts", "xfoo.ts", "abd", "acd", "ad", "a", "b", "bb",
	"bc", "ab", "ac", "(a", "1", "2", "9", "12", "3", "z", "y", "[abc", "a{b", "a{b,c", "a{b}", "a}b",
	"src/foo.ts", "src/.ts", "src/a.ts", "src/deep/nested/file.ts", "src/deep/nested/file.tsx",
	"abc", "abbc", "a1", "b2", "a(b", "a)b", "aa", "ax", "by", "ac", "axc", "aaa", "abab", "cc", "b3",
	"x/foo", "x/bar", "x/baz", "x/foo/child", "x/a.d.ts", "x/a.ts", "a[abc]", "a[!x]", "a!", "aa/", "a/",
	"ay", "az", "a0", "b9", "aZ", "a*", "b?", 'a"*"', "*", "a[]", "a[^]", "a[z-a]", "a-", "a[]/", "[]",
	"[^]", "[z-a]", "!", "a[", "a]", "a/", "a[!x]", "a[a/b]", "a^", "a~", "a&", "a[a&&b]", "a[a~b]",
	"", ".", "..", "x/.", "x/..", "x/a", "a/.", "a/..", "a/x", "a/x/y", "a/b", "ab/", "a/b/", "a/",
	"/a", "a/x/../b", "a{bmore", "a{x}", "a3", "a:", "as]", "a[]", "a b", "aa ", "a|", "a[a~~b]",
	"\ud83d\ude00", "a\ud83d\ude00", "\n", "a\n", "\r", "\u2028", "\u2029", "\ud83d\ude00\ud83d\ude00",
	"b\ud83d\ude00", "a\n\ud83d\ude00", "a:b", "a::b", "a:.ts", "a:]",
	// Literal texts of the astral extglob patterns above, so the pinned escaped-literal
	// fallback (risky extglob without a flat safe output) is exercised too.
	"+(*(a)|\ud83d\ude00)", "+(*(\ud83d\ude00))", "+(\ud83d\ude00|\ud83d\ude00)", "\ud83d\ude00a", "a\ud83d\ude00b",
	"a.b", "a.b.c", "a.ts", "foo.ts", "CONTEXT.md", "file.rs", "file.rs.bak", "a/x/b", "ax/b",
	// Astral-range endpoints and the single units a brace-range class selects.
	"\u{e000}", "\u{e001}", "\ud83d\ude01", "A", "e", "8", "9", "<", ">", "\u0000", "\u0003", "az\u0003b",
];
const exclusions = ["!foo", "!!foo", "!@(a|b)", "![z-a]", '!"*"', "!**/foo.ts", "!foo/*", "!*/*.ts"];

// Grammar-level cases: patterns whose meaning the consumer's `normalizePath` (`\` -> `/`)
// would destroy before picomatch sees them. The Rust tests feed these to the ported
// `picomatch` module directly, so they are compared against the pinned picomatch without
// the consumer normalization.
const grammarPatterns = [
	"\\a", "a\\z", "\\3", "\\A", "\\e", "\\8", "\\9", "\\<", "\\>", "\\0", "\\12", "\\cA", "\\x41",
	"a\\z\\3b", "\\z\\a", "\\_", "\\ ", "\\-", "\\1", "\\377", "\\b", "\\d",
];
const grammarPaths = [
	"a", "z", "A", "e", "8", "9", "<", ">", "_", " ", "-", "1", "3", "x", "az\u0003b",
	"\u0000", "\u0001", "\u0003", "\n", "azb", "\u00ff", "aa", "zz", "ab", "b", "d",
];

function hashFile(path) {
	return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function generate() {
	const { picomatch, version, packagePath, packageDir, libDir } = pinnedPicomatch();
	const cases = [];
	for (const pattern of patterns) {
		let matches;
		try {
			matches = picomatch(pattern, OPTIONS);
		} catch {
			continue;
		}
		for (const path of paths) cases.push({ pattern, path, matched: Boolean(matches(path)), exclusion: false });
	}
	for (const pattern of exclusions) {
		const positive = picomatch("**", OPTIONS);
		let excluded;
		try {
			excluded = picomatch(pattern.slice(1), OPTIONS);
		} catch {
			continue;
		}
		for (const path of paths) cases.push({ pattern, path, matched: positive(path) && !excluded(path), exclusion: true });
	}
	const grammarCases = [];
	for (const pattern of grammarPatterns) {
		let matches;
		try {
			matches = picomatch(pattern, OPTIONS);
		} catch {
			continue;
		}
		for (const path of grammarPaths) grammarCases.push({ pattern, path, matched: Boolean(matches(path)) });
	}
	const provenance = {
		source: "picomatch",
		version,
		options: OPTIONS,
		packagePath,
		packageDir,
		libHashes: Object.fromEntries([
			["index.js", hashFile(join(packageDir, "index.js"))],
			...["parse.js", "constants.js", "scan.js", "picomatch.js", "utils.js"].map((name) => [
				`lib/${name}`,
				hashFile(join(libDir, name)),
			]),
		]),
		caseCount: cases.length,
		grammarCaseCount: grammarCases.length,
	};
	return { payload: `${JSON.stringify({ provenance, cases, grammarCases }, null, 1)}\n`, provenance };
}

const { payload, provenance } = generate();
for (const crate of CRATES) writeFileSync(join(repoRoot, crate, "tests", "latest-omo-rules-matcher.json"), payload);
console.log(`latest-omo-rules-matcher: ${provenance.caseCount} source-derived cases + ${provenance.grammarCaseCount} grammar cases written for ${CRATES.length} crates`);
