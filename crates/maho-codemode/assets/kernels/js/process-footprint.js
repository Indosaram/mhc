import { readFileSync } from "node:fs";

let reader;

export function readProcessFootprint() {
	try {
		reader ??= createReader();
		return reader();
	} catch {
		return undefined;
	}
}

function createReader() {
	if (process.platform === "linux") return () => {
		const match = /^RssAnon:\s+(\d+)\s+kB$/m.exec(readFileSync(`/proc/${process.pid}/status`, "utf8"));
		return match === null ? undefined : Number(match[1]) * 1024;
	};
	const { dlopen, FFIType, ptr } = process.getBuiltinModule("bun:ffi");
	if (process.platform === "darwin") {
		const library = dlopen("libSystem.B.dylib", {
			proc_pid_rusage: { args: [FFIType.i32, FFIType.i32, FFIType.ptr], returns: FFIType.i32 },
		});
		const buffer = new Uint8Array(256);
		return () => {
			buffer.fill(0);
			if (library.symbols.proc_pid_rusage(process.pid, 2, ptr(buffer)) !== 0) return undefined;
			return Number(new DataView(buffer.buffer).getBigUint64(72, true));
		};
	}
	if (process.platform === "win32") {
		const library = dlopen("kernel32.dll", {
			OpenProcess: { args: [FFIType.u32, FFIType.i32, FFIType.u32], returns: FFIType.ptr },
			K32GetProcessMemoryInfo: { args: [FFIType.ptr, FFIType.ptr, FFIType.u32], returns: FFIType.i32 },
			CloseHandle: { args: [FFIType.ptr], returns: FFIType.i32 },
		});
		const buffer = new Uint8Array(80);
		return () => {
			const handle = library.symbols.OpenProcess(0x1000, 0, process.pid);
			if (!handle) return undefined;
			try {
				buffer.fill(0);
				if (library.symbols.K32GetProcessMemoryInfo(handle, ptr(buffer), buffer.byteLength) === 0) return undefined;
				return Number(new DataView(buffer.buffer).getBigUint64(72, true));
			} finally {
				library.symbols.CloseHandle(handle);
			}
		};
	}
	return () => undefined;
}
