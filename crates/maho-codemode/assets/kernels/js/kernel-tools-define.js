export function createToolNamespace(define, callHost) {
	const registrar = function tool(fn, metadata) {
		return define(fn, metadata);
	};
	return new Proxy(registrar, {
		get(target, prop) {
			if (typeof prop !== "string") return undefined;
			if (prop in Function.prototype || prop === "arguments" || prop === "caller") return target[prop];
			return async (args) => await callHost(prop, args ?? {});
		},
	});
}
