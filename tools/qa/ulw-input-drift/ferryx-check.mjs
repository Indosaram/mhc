#!/usr/bin/env bun
// A nested PTY never traverses the operator's Ferryx output transport.
// No documented Ferryx probe/automation interface is present in the evidence.
export function detectFerryx() {
  return {
    status: "ferryx-unavailable",
    verified: false,
    reason: "This driver owns a local PTY, not a Ferryx client/session capture. Environment variables and socket presence cannot verify the remote surface.",
    manual_procedure: "tools/qa/ulw-input-drift/MANUAL-FERRYX.md",
  };
}

if (import.meta.main) {
  console.log(JSON.stringify(detectFerryx()));
  process.exitCode = 77;
}
