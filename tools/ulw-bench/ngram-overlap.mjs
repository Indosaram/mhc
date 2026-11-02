// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

/**
 * Tokenize input text into lowercase alphanumeric word tokens.
 * Strips punctuation, quotes, markdown formatting, and symbols.
 *
 * @param {string} text
 * @returns {string[]}
 */
export function tokenizeWords(text) {
	if (!text || typeof text !== "string") {
		return [];
	}
	const tokens = text.toLowerCase().match(/[\p{L}\p{N}]+/gu);
	return tokens ?? [];
}

/**
 * Extract consecutive word n-grams from an array of tokens.
 *
 * @param {string[]} words
 * @param {number} n
 * @returns {string[]}
 */
export function extractNgrams(words, n) {
	if (!Array.isArray(words) || words.length < n || n <= 0) {
		return [];
	}
	const ngrams = [];
	for (let i = 0; i <= words.length - n; i++) {
		ngrams.push(words.slice(i, i + n).join(" "));
	}
	return ngrams;
}

/**
 * Find all n-grams from textA that appear in textB.
 *
 * @param {string} textA - candidate text (e.g. spec)
 * @param {string} textB - reference text (e.g. SUL directive)
 * @param {number} [n=8] - n-gram size in words
 * @param {object} [options={}]
 * @param {boolean} [options.perLine=false] - tokenize textA line by line
 * @returns {Array<{ gram: string, line?: number }>}
 */
export function findOverlap(textA, textB, n = 8, options = {}) {
	const wordsB = tokenizeWords(textB);
	const bNgramSet = new Set(extractNgrams(wordsB, n));
	if (bNgramSet.size === 0) {
		return [];
	}

	if (options.perLine) {
		const lines = textA.split(/\r?\n/);
		const matches = [];
		for (let i = 0; i < lines.length; i++) {
			const line = lines[i] ?? "";
			const wordsA = tokenizeWords(line);
			const aNgrams = extractNgrams(wordsA, n);
			for (const gram of aNgrams) {
				if (bNgramSet.has(gram)) {
					matches.push({ gram, line: i + 1 });
				}
			}
		}
		return matches;
	}

	const wordsA = tokenizeWords(textA);
	const aNgrams = extractNgrams(wordsA, n);
	const matches = [];
	for (const gram of aNgrams) {
		if (bNgramSet.has(gram)) {
			matches.push({ gram });
		}
	}
	return matches;
}

/**
 * Main CLI runner.
 * Usage: node ngram-overlap.mjs [--n <num>] [--per-line] [--json] <fileA> <fileB>
 */
export function runCli(argv = process.argv.slice(2)) {
	let n = 8;
	let perLine = false;
	let jsonOutput = false;
	const files = [];

	for (let i = 0; i < argv.length; i++) {
		const arg = argv[i];
		if (arg === "--n" || arg === "-n") {
			const next = argv[++i];
			if (!next) {
				console.error("Missing value for --n");
				process.exit(2);
			}
			n = Number.parseInt(next, 10);
			if (Number.isNaN(n) || n <= 0) {
				console.error(`Invalid --n value: ${next}`);
				process.exit(2);
			}
		} else if (arg.startsWith("--n=")) {
			n = Number.parseInt(arg.slice(4), 10);
			if (Number.isNaN(n) || n <= 0) {
				console.error(`Invalid --n value: ${arg}`);
				process.exit(2);
			}
		} else if (arg === "--per-line") {
			perLine = true;
		} else if (arg === "--json") {
			jsonOutput = true;
		} else if (arg === "--help" || arg === "-h") {
			console.log(
				"Usage: node ngram-overlap.mjs [--n 8] [--per-line] [--json] <fileA> <fileB>",
			);
			process.exit(0);
		} else if (!arg.startsWith("-")) {
			files.push(arg);
		} else {
			console.error(`Unknown option: ${arg}`);
			process.exit(2);
		}
	}

	if (files.length !== 2) {
		console.error(
			"Error: exactly two file arguments are required: <fileA> <fileB>",
		);
		process.exit(2);
	}

	const fileA = resolve(process.cwd(), files[0]);
	const fileB = resolve(process.cwd(), files[1]);

	let textA = "";
	let textB = "";
	try {
		textA = readFileSync(fileA, "utf8");
	} catch (err) {
		console.error(`Failed to read fileA (${fileA}): ${err.message}`);
		process.exit(2);
	}
	try {
		textB = readFileSync(fileB, "utf8");
	} catch (err) {
		console.error(`Failed to read fileB (${fileB}): ${err.message}`);
		process.exit(2);
	}

	const matches = findOverlap(textA, textB, n, { perLine });

	if (jsonOutput) {
		console.log(
			JSON.stringify(
				{
					n,
					fileA,
					fileB,
					overlapCount: matches.length,
					matches,
				},
				null,
				2,
			),
		);
	} else {
		console.log(matches.length);
		if (matches.length > 0) {
			const sample = matches.slice(0, 5).map((m) => m.gram);
			console.error(
				`Found ${matches.length} matching ${n}-gram(s). Samples:\n  - ${sample.join("\n  - ")}`,
			);
		}
	}

	if (matches.length > 0) {
		process.exit(1);
	}
	process.exit(0);
}

const isDirectCall =
	process.argv[1] &&
	resolve(process.argv[1]) === fileURLToPath(import.meta.url);

if (isDirectCall) {
	runCli();
}
