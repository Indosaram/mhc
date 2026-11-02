// SPDX-License-Identifier: MIT
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
	extractNgrams,
	findOverlap,
	tokenizeWords,
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

	it("handles unicode and korean text properly", () => {
		const text = "테스트 문장입니다. Model evaluation testing 100%.";
		const tokens = tokenizeWords(text);
		assert.deepEqual(tokens, [
			"테스트",
			"문장입니다",
			"model",
			"evaluation",
			"testing",
			"100",
		]);
	});
});

describe("extractNgrams", () => {
	it("returns empty array when token count is less than n", () => {
		const words = ["one", "two", "three"];
		assert.deepEqual(extractNgrams(words, 4), []);
		assert.deepEqual(extractNgrams([], 1), []);
		assert.deepEqual(extractNgrams(words, 0), []);
	});

	it("extracts sliding window of n words", () => {
		const words = ["a", "b", "c", "d", "e"];
		const ngrams = extractNgrams(words, 3);
		assert.deepEqual(ngrams, ["a b c", "b c d", "c d e"]);
	});
});

describe("findOverlap", () => {
	it("detects 8-word overlaps between two texts", () => {
		const textA =
			"The quick brown fox jumps over the lazy dog and runs away.";
		const textB =
			"In the morning, the quick brown fox jumps over the lazy dog was seen.";
		const matches = findOverlap(textA, textB, 8);
		assert.equal(matches.length, 2);
		assert.equal(matches[0].gram, "the quick brown fox jumps over the lazy");
		assert.equal(matches[1].gram, "quick brown fox jumps over the lazy dog");
	});

	it("returns empty when no 8-word run is shared", () => {
		const textA = "Completely distinct formulation of operational guidelines and constraints.";
		const textB = "Different concepts applied here with unique wording and phrasing.";
		const matches = findOverlap(textA, textB, 8);
		assert.equal(matches.length, 0);
	});

	it("supports perLine option and attributes line numbers", () => {
		const textA =
			"First line here.\nAlpha one two three four five six seven.\nThird line.";
		const textB = "Here is alpha one two three four five six seven text.";
		const matches = findOverlap(textA, textB, 8, { perLine: true });
		assert.equal(matches.length, 1);
		assert.equal(
			matches[0].gram,
			"alpha one two three four five six seven",
		);
		assert.equal(matches[0].line, 2);
	});
});
