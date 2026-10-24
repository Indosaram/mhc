#!/usr/bin/env bun
import { createHash } from "node:crypto";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { basename, resolve } from "node:path";

export function detectTargetTuple() {
	if (process.platform === "linux" && process.arch === "x64") {
		return "linux-x86_64";
	}
	if (process.platform === "linux" && process.arch === "arm64") {
		return "linux-aarch64";
	}
	if (process.platform === "darwin" && process.arch === "arm64") {
		return "macos-aarch64";
	}
	if (process.platform === "darwin" && process.arch === "x64") {
		return "macos-x86_64";
	}
	if (process.platform === "win32" && process.arch === "x64") {
		return "windows-x86_64";
	}
	return `${process.platform}-${process.arch}`;
}

export function getTargetExt(targetTuple) {
	if (targetTuple.includes("windows")) return "zip";
	return "tar.gz";
}

export const CORRUPT_ARCHIVE_BYTES = Buffer.from("NOT_A_VALID_GZIP_OR_ZIP_PACKAGE_CONTENT\n");
export const CORRUPT_ARCHIVE_SHA256 = createHash("sha256").update(CORRUPT_ARCHIVE_BYTES).digest("hex");

export function computeFileSha256(filePath) {
	if (!filePath || !existsSync(filePath)) return null;
	const buffer = readFileSync(filePath);
	return createHash("sha256").update(buffer).digest("hex");
}

export function createUpdateFixtureServer(options = {}) {
	let currentMode = options.mode ?? "normal";
	let currentVersion = options.version;
	if (!currentVersion) throw new Error("Fixture requires an explicit release version");
	let currentPackagePath = options.packagePath ? resolve(options.packagePath) : null;
	let currentTarget = options.target ?? detectTargetTuple();
	const requests = [];

	const server = Bun.serve({
		hostname: options.hostname ?? "127.0.0.1",
		port: options.port ?? 0,
		async fetch(request) {
			const url = new URL(request.url);
			const entry = {
				url: request.url,
				pathname: url.pathname,
				method: request.method,
				timestamp: Date.now(),
				mode: currentMode,
			};
			requests.push(entry);

			if (currentMode === "rate_limited") {
				return new Response(JSON.stringify({ message: "API rate limit exceeded" }), {
					status: 403,
					headers: {
						"content-type": "application/json",
						"x-ratelimit-remaining": "0",
					},
				});
			}

			if (currentMode === "server_error") {
				return new Response("Internal Server Error", { status: 500 });
			}

			const isReleaseEndpoint =
				url.pathname.includes("/releases/latest") ||
				url.pathname.endsWith("/latest") ||
				url.pathname === "/";

			if (isReleaseEndpoint && request.method === "GET") {
				const tag = `v${currentVersion}`;
				const baseUrl = `http://${server.hostname}:${server.port}`;
				const ext = getTargetExt(currentTarget);
				const assetName = `mhc-${currentTarget}.${ext}`;
				const assets = [
					{
						name: assetName,
						browser_download_url: `${baseUrl}/assets/${assetName}`,
					},
					{
						name: `${assetName}.sha256`,
						browser_download_url: `${baseUrl}/assets/${assetName}.sha256`,
					},
				];
				if (currentPackagePath && existsSync(`${currentPackagePath}.minisig`)) {
					assets.push({
						name: `${assetName}.minisig`,
						browser_download_url: `${baseUrl}/assets/${assetName}.minisig`,
					});
				}

				const releasePayload = {
					tag_name: tag,
					name: `mhc ${tag}`,
					draft: false,
					prerelease: false,
					html_url: `https://github.com/Indosaram/mhc/releases/tag/${tag}`,
					assets,
				};
				return new Response(JSON.stringify(releasePayload, null, 2), {
					status: 200,
					headers: { "content-type": "application/json" },
				});
			}

			if (url.pathname.startsWith("/assets/")) {
				const requestedFile = url.pathname.slice("/assets/".length);
				const expectedExt = getTargetExt(currentTarget);
				const expectedAssetName = `mhc-${currentTarget}.${expectedExt}`;

				if (requestedFile.endsWith(".minisig")) {
					const sigPath = currentPackagePath ? `${currentPackagePath}.minisig` : null;
					if (requestedFile !== `${expectedAssetName}.minisig` || !sigPath || !existsSync(sigPath)) {
						return new Response("Asset Not Found", { status: 404 });
					}
					return new Response(Bun.file(sigPath), { status: 200, headers: { "content-type": "text/plain" } });
				}

				if (requestedFile.endsWith(".sha256")) {
					const archiveName = requestedFile.slice(0, -".sha256".length);
					if (archiveName !== expectedAssetName) {
						return new Response("Asset Not Found", { status: 404 });
					}
					if (currentMode === "bad_checksum") {
						const badHash = "deadbeef".repeat(8);
						return new Response(`${badHash}  ${archiveName}\n`, {
							status: 200,
							headers: { "content-type": "text/plain" },
						});
					}
					if (currentMode === "invalid_archive") {
						return new Response(`${CORRUPT_ARCHIVE_SHA256}  ${archiveName}\n`, {
							status: 200,
							headers: { "content-type": "text/plain" },
						});
					}
					if (!currentPackagePath || !existsSync(currentPackagePath)) {
						return new Response("Asset Not Found", { status: 404 });
					}
					const hash = computeFileSha256(currentPackagePath);
					return new Response(`${hash}  ${archiveName}\n`, {
						status: 200,
						headers: { "content-type": "text/plain" },
					});
				}

				if (currentMode === "invalid_archive") {
					if (requestedFile !== expectedAssetName) {
						return new Response("Asset Not Found", { status: 404 });
					}
					return new Response(CORRUPT_ARCHIVE_BYTES, {
						status: 200,
						headers: { "content-type": "application/octet-stream" },
					});
				}

				if (requestedFile !== expectedAssetName) {
					return new Response("Asset Not Found", { status: 404 });
				}

				if (currentPackagePath && existsSync(currentPackagePath)) {
					const file = Bun.file(currentPackagePath);
					return new Response(file, {
						status: 200,
						headers: {
							"content-type": "application/octet-stream",
							"content-disposition": `attachment; filename="${requestedFile}"`,
						},
					});
				}

				return new Response("Asset Not Found", { status: 404 });
			}

			return new Response("Not Found", { status: 404 });
		},
	});

	return {
		server,
		get hostname() {
			return server.hostname;
		},
		get port() {
			return server.port;
		},
		get url() {
			return `http://${server.hostname}:${server.port}`;
		},
		get target() {
			return currentTarget;
		},
		get releaseUrl() {
			return `http://${server.hostname}:${server.port}/repos/Indosaram/mhc/releases/latest`;
		},
		get mode() {
			return currentMode;
		},
		get version() {
			return currentVersion;
		},
		setMode(newMode) {
			currentMode = newMode;
		},
		setVersion(newVersion) {
			currentVersion = newVersion;
		},
		setTarget(newTarget) {
			currentTarget = newTarget;
		},
		setPackage(packagePath, version = null) {
			currentPackagePath = resolve(packagePath);
			if (version) {
				currentVersion = version;
			}
		},
		getRequestCount() {
			return requests.length;
		},
		getRequests() {
			return [...requests];
		},
		clearRequests() {
			requests.length = 0;
		},
		stop() {
			server.stop(true);
		},
	};
}

if (import.meta.main) {
	if (process.argv[2] === "--write-rollback-probe") {
		// A real native executable: staging succeeds, installed validation fails.
		// No updater test hook or filesystem race is needed on any platform.
		writeFileSync(process.argv[3], `fn main() -> Result<(), Box<dyn std::error::Error>> {
    let executable = std::env::current_exe()?;
    let installed = executable.parent().and_then(|p| p.parent()).and_then(|p| p.file_name()).is_some_and(|p| p == "versions");
    if installed {
        std::fs::write(std::env::var("MHC_QA_ROLLBACK_MARKER")?, executable.to_string_lossy().as_bytes())?;
        std::process::exit(73);
    }
    println!("{}", std::env::var("MHC_QA_PROBE_VERSION")?);
    Ok(())
}
`);
		process.exit(0);
	}
	const args = process.argv.slice(2);
	const parsed = {
		port: 0,
		packagePath: null,
		version: null,
		mode: "normal",
	};
	for (let i = 0; i < args.length; i++) {
		if (args[i] === "--port" && i + 1 < args.length) {
			parsed.port = Number.parseInt(args[++i], 10);
		} else if (args[i] === "--package" && i + 1 < args.length) {
			parsed.packagePath = args[++i];
		} else if (args[i] === "--version" && i + 1 < args.length) {
			parsed.version = args[++i];
		} else if (args[i] === "--mode" && i + 1 < args.length) {
			parsed.mode = args[++i];
		}
	}

	const fixture = createUpdateFixtureServer(parsed);
	console.log(
		JSON.stringify({
			status: "ready",
			port: fixture.port,
			url: fixture.url,
			releaseUrl: fixture.releaseUrl,
			mode: fixture.mode,
			version: fixture.version,
		}),
	);

	process.on("SIGINT", () => {
		fixture.stop();
		process.exit(0);
	});
	process.on("SIGTERM", () => {
		fixture.stop();
		process.exit(0);
	});
}
