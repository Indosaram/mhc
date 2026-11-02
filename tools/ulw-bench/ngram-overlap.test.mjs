// SPDX-License-Identifier: MIT
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { readFileSync, writeFileSync, mkdirSync, rmSync } from "node:fs";
import { join } from "node:path";
import {
	extractNgrams,
	findOverlap,
	tokenizeWords,
	findFiles,
} from "./ngram-overlap.mjs";

describe("tokenizeWords", () => {
	it("returns empty array for empty or non-string input", () => {
		assert.deepEqual(tokenizeWords(""), []);
		assert.deepEqual(tokenizeWords(null), []);
		assert.deepEqual(tokenizeWords(undefined), []);
	});

	it("tokenizes words and removes punctuation and symbols", () => {
		const text = "Hello, world! This is a test: 123 (with symbols & punctuation).";
		const tokens = tokenizeWords(text);
		assert.deepEqual(tokens, [
			"hello",
			"world",
			"this",
			"is",
			"a",
			"test",
			"123",
			"with",
			"symbols",
			"punctuation",
		]);
	});
});

describe("extractNgrams", () => {
	it("returns empty array when token count is less than n", () => {
		const words = ["one", "two", "three"];
		assert.deepEqual(extractNgrams(words, 4), []);
	});

	it("extracts sliding window of n words", () => {
		const words = ["one", "two", "three", "four"];
		assert.deepEqual(extractNgrams(words, 2), ["one two", "two three", "three four"]);
	});
});

describe("findOverlap", () => {
	it("detects 8-word overlaps between two texts", () => {
		const textA = "alpha beta gamma delta epsilon zeta eta theta";
		const textB = "mu nu alpha beta gamma delta epsilon zeta eta theta rho";
		const matches = findOverlap(textA, textB, 8);
		assert.equal(matches.length, 1);
		assert.equal(matches[0].gram, "alpha beta gamma delta epsilon zeta eta theta");
	});
});

describe("findFiles", () => {
	const testDir = "./temp-test-dir";
	it("finds files recursively", () => {
		mkdirSync(testDir, { recursive: true });
		writeFileSync(join(testDir, "a.txt"), "hello");
		mkdirSync(join(testDir, "sub"));
		writeFileSync(join(testDir, "sub", "b.txt"), "world");
		
		const files = findFiles(testDir);
		assert.ok(files.includes(join(testDir, "a.txt")));
		assert.ok(files.includes(join(testDir, "sub", "b.txt")));
		
		rmSync(testDir, { recursive: true });
	});
    
	it("returns single file", () => {
		writeFileSync("single.txt", "content");
		const files = findFiles("single.txt");
		assert.deepEqual(files, ["single.txt"]);
		rmSync("single.txt");
	});
});
