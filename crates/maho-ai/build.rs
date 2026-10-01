//! Compiles the vendored protobuf schemas into prost modules at build time:
//! - `proto/agent.proto` (from senpi's `packages/ai/proto/cursor/agent.proto`, pinned commit
//!   fe8c564b) into the `agent.v1` module, included by
//!   `src/api/cursor_agent/gen/agent_pb.rs`;
//! - `proto/devin/cascade.proto` (from senpi's `packages/ai/proto/devin/cascade.proto`, pinned
//!   commit fe8c564b) into the `exa.api_server_pb` module, included by
//!   `src/api/devin_agent/gen/cascade_pb.rs`.
//!
//! Both schemas are vendored copies of the pinned senpi sources, so the wire encoding stays
//! derived from the same schema the TS client generates from; see each `gen/` module for how
//! message/enum names map onto senpi's generated TypeScript.

fn main() {
    println!("cargo:rerun-if-changed=proto/agent.proto");
    println!("cargo:rerun-if-changed=proto/devin/cascade.proto");

    let mut config = prost_build::Config::new();
    // senpi's protoc-gen-es client treats every field as present-or-default
    // (proto3 semantics); prost's default codegen already matches that for
    // scalars. `optional` fields in the .proto become `Option<T>` by default
    // in prost 0.14, matching the `?:`-style optionality baked into the TS
    // client's generated types.
    config.bytes(["."]);

    // Map fields become BTreeMap so encoding is deterministic. prost's default
    // HashMap iterates in a per-process random order, which would make the
    // golden round-trip tests (byte-identity against senpi's protobuf-es
    // output) flaky for any map with more than one entry. The golden generator
    // writes map entries in sorted key order, matching BTreeMap, so both
    // encoders agree on the byte sequence.
    config.btree_map(["."]);

    config
        .compile_protos(&["proto/agent.proto", "proto/devin/cascade.proto"], &["proto"])
        .expect("failed to compile the vendored protobuf schemas under crates/maho-ai/proto");
}
