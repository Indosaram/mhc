// JSON remains the host protocol; a BigInt or an exotic object (Date, Map, class
// instance) travels as a marker the host revives, as in upstream senpi's frames.
import { serialize, deserialize } from "node:v8";

const CLONE = "\0senpi:clone";
const BIGINT = "\0senpi:bigint";
// A plain object that happens to hold exactly one marker key must survive the round
// trip as data, so encoding escapes it under this key and decoding restores it.
const LITERAL = "\0senpi:literal";

function isPlainObject(value) {
	return value !== null && typeof value === "object" && !Array.isArray(value)
		&& Object.getPrototypeOf(value) === Object.prototype;
}

function normalize(value) {
	if (typeof value === "bigint") return { [BIGINT]: value.toString() };
	if (value === undefined) return undefined;
	if (value === null || typeof value !== "object") return value;
	if (Array.isArray(value)) return value.map((item) => normalize(item) ?? null);
	if (!isPlainObject(value)) return { [CLONE]: serialize(value).toString("base64") };
	if (Object.keys(value).length === 1) {
		const only = Object.keys(value)[0];
		if (only === CLONE || only === BIGINT || only === LITERAL) {
			return { [LITERAL]: normalize(value[only]) };
		}
	}
	const out = {};
	for (const [key, item] of Object.entries(value)) {
		if (item === undefined) continue;
		out[key] = normalize(item);
	}
	return out;
}

export function encodeFrameValues(message) {
	return Object.fromEntries(Object.entries(message).map(([key, value]) => [key, normalize(value)]));
}

export function reviveFrameValues(value) {
	if (value === null || typeof value !== "object") return value;
	if (!Array.isArray(value) && Object.keys(value).length === 1) {
		if (Object.hasOwn(value, LITERAL)) return reviveFrameValues(value[LITERAL]);
		// A hand-built marker that fails to decode is data, not control: return it untouched
		// instead of throwing on the frame path, where no caller catches.
		if (Object.hasOwn(value, CLONE)) {
			try {
				return deserialize(Buffer.from(value[CLONE], "base64"));
			} catch {
				return value;
			}
		}
		if (Object.hasOwn(value, BIGINT)) {
			try {
				return BigInt(value[BIGINT]);
			} catch {
				return value;
			}
		}
	}
	// Assignment, not a JSON.parse reviver, so markers revive in place at any depth.
	for (const key of Object.keys(value)) value[key] = reviveFrameValues(value[key]);
	return value;
}
