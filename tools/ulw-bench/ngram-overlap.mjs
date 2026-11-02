// SPDX-License-Identifier: MIT
import { readFileSync, lstatSync, readdirSync, existsSync } from "node:fs";
import { resolve, extname, join } from "node:path";

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
 * Recursively find all text files in a path.
 *
 * @param {string} path
 * @returns {string[]}
 */
export function findFiles(path) {
	const stats = lstatSync(path);
	if (stats.isFile()) {
		return [path];
	}
	let files = [];
	for (const entry of readdirSync(path)) {
		const fullPath = join(path, entry);
		if (lstatSync(fullPath).isDirectory()) {
			files = files.concat(findFiles(fullPath));
		} else {
			files.push(fullPath);
		}
	}
	return files;
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
 * Usage: node ngram-overlap.mjs <fileOrDirA> <fileOrDirB>
 */
export function runCli() {
	const argv = process.argv.slice(2);
	if (argv.length !== 2) {
		console.error("Usage: node ngram-overlap.mjs <fileOrDirA> <fileOrDirB>");
		process.exit(2);
	}

	const filesA = findFiles(argv[0]);
	const filesB = findFiles(argv[1]);

	if (filesA.length === 0 || filesB.length === 0) {
		console.error("No files found in source sets.");
		process.exit(2);
	}

	let totalWordsA = 0;
	const textA = filesA.map(f => {
		const content = readFileSync(f, "utf8");
		totalWordsA += tokenizeWords(content).length;
		return content;
	}).join("\n");
	
	const textB = filesB.map(f => readFileSync(f, "utf8")).join("\n");
	const totalWordsB = tokenizeWords(textB).length;

	if (totalWordsA === 0 || totalWordsB === 0) {
		console.error("No words found in source sets.");
		process.exit(2);
	}

	const matches = findOverlap(textA, textB, 8);
	if (matches.length > 0) {
		console.log(`Found ${matches.length} matches.`);
		process.exit(1);
	}
	console.log("No matches found.");
	process.exit(0);
}

if (process.argv[1] && process.argv[1].endsWith("ngram-overlap.mjs")) {
    runCli();
}

