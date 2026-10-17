import { types } from "node:util";

const SAMPLED_ELEMENTS = 1_000;
const NODE_BUDGET = 200_000;
const MAX_DEPTH = 64;
const POINTER_BYTES = 8;
const OBJECT_BYTES = 16;
const FUNCTION_BYTES = 64;
const INTERNAL_GLOBAL_PREFIX = "__senpi";
const MIN_REPORTED_BYTES = 1024 * 1024;
const getter = (prototype, name) => Object.getOwnPropertyDescriptor(prototype, name)?.get;
const TYPED_ARRAY_BYTE_LENGTH = getter(Object.getPrototypeOf(Uint8Array.prototype), "byteLength");
const DATA_VIEW_BYTE_LENGTH = getter(DataView.prototype, "byteLength");
const ARRAY_BUFFER_BYTE_LENGTH = getter(ArrayBuffer.prototype, "byteLength");
const SHARED_ARRAY_BUFFER_BYTE_LENGTH = typeof SharedArrayBuffer === "function" ? getter(SharedArrayBuffer.prototype, "byteLength") : undefined;
const MAP_SIZE = getter(Map.prototype, "size");
const SET_SIZE = getter(Set.prototype, "size");
const MAP_ENTRIES = Map.prototype.entries;
const SET_VALUES = Set.prototype.values;
const MAP_NEXT = Object.getPrototypeOf(new Map().entries()).next;
const SET_NEXT = Object.getPrototypeOf(new Set().values()).next;
const read = (fn, value) => Reflect.apply(fn, value, []);

export function captureGlobalBaseline() { return new Set(Object.getOwnPropertyNames(globalThis)); }

export function largestGlobals(baseline, limit) {
	const sized = [];
	const seen = new WeakSet();
	let nodes = 0;
	let approximate = false;
	function size(value, depth = 0) {
		switch (typeof value) {
			case "string": return OBJECT_BYTES + value.length * 2;
			case "function": return FUNCTION_BYTES;
			case "object": break;
			default: return 0;
		}
		if (value === null || seen.has(value)) return 0;
		if (types.isProxy(value)) return OBJECT_BYTES;
		if (depth >= MAX_DEPTH || nodes++ >= NODE_BUDGET) { approximate = true; return 0; }
		seen.add(value);
		if (types.isTypedArray(value)) return read(TYPED_ARRAY_BYTE_LENGTH, value);
		if (types.isDataView(value)) return read(DATA_VIEW_BYTE_LENGTH, value);
		if (types.isArrayBuffer(value)) return read(ARRAY_BUFFER_BYTE_LENGTH, value);
		if (types.isSharedArrayBuffer(value) && SHARED_ARRAY_BUFFER_BYTE_LENGTH) return read(SHARED_ARRAY_BUFFER_BYTE_LENGTH, value);
		if (Array.isArray(value)) {
			const length = value.length;
			let total = 0;
			const step = Math.max(1, Math.ceil(length / SAMPLED_ELEMENTS));
			for (let i = 0; i < length; i += step) {
				const d = Object.getOwnPropertyDescriptor(value, i);
				if (d && "value" in d) total += size(d.value, depth + 1);
				else if (d) approximate = true;
			}
			if (length > SAMPLED_ELEMENTS) { total *= length / Math.ceil(length / step); approximate = true; }
			return OBJECT_BYTES + POINTER_BYTES * length + total;
		}
		if (types.isMap(value) || types.isSet(value)) {
			const isMap = types.isMap(value);
			const count = read(isMap ? MAP_SIZE : SET_SIZE, value);
			const iterator = Reflect.apply(isMap ? MAP_ENTRIES : SET_VALUES, value, []);
			const next = isMap ? MAP_NEXT : SET_NEXT;
			let total = 0, taken = 0;
			while (taken < Math.min(count, SAMPLED_ELEMENTS)) {
				const step = Reflect.apply(next, iterator, []); if (step.done) break;
				total += isMap ? size(step.value[0], depth + 1) + size(step.value[1], depth + 1) : size(step.value, depth + 1); taken++;
			}
			if (taken < count) approximate = true;
			return OBJECT_BYTES + POINTER_BYTES * count * (isMap ? 2 : 1) + (taken ? total * count / taken : 0);
		}
		const keys = Object.keys(value);
		let total = 0;
		for (const key of keys.slice(0, SAMPLED_ELEMENTS)) {
			const d = Object.getOwnPropertyDescriptor(value, key);
			if (d && "value" in d) total += size(d.value, depth + 1);
			else if (d) approximate = true;
		}
		if (keys.length > SAMPLED_ELEMENTS) { total *= keys.length / SAMPLED_ELEMENTS; approximate = true; }
		return OBJECT_BYTES + POINTER_BYTES * keys.length + total;
	}
	for (const name of Object.getOwnPropertyNames(globalThis)) {
		if (baseline.has(name) || name.startsWith(INTERNAL_GLOBAL_PREFIX)) continue;
		const descriptor = Object.getOwnPropertyDescriptor(globalThis, name);
		if (!descriptor || !("value" in descriptor)) continue;
		approximate = false;
		try {
			const bytes = Math.round(POINTER_BYTES + size(descriptor.value));
			if (bytes >= MIN_REPORTED_BYTES) sized.push({ name, bytes, ...(approximate ? { approximate: true } : {}) });
		} catch { /* A forged built-in object is not reportable. */ }
	}
	return sized.sort((a, b) => b.bytes - a.bytes).slice(0, limit);
}
