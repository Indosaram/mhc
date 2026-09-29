//! Compiles `proto/agent.proto` (vendored from senpi's
//! `packages/ai/proto/cursor/agent.proto`, pinned commit fe8c564b) into the
//! `agent.v1` prost module. The generated code is included by
//! `src/api/cursor_agent/gen/agent_pb.rs` via `include!(...)`; see that file
//! for how message/enum names map onto senpi's `gen/agent_pb.ts`.

fn main() {
    println!("cargo:rerun-if-changed=proto/agent.proto");

    let mut config = prost_build::Config::new();
    // senpi's protoc-gen-es client treats every field as present-or-default
    // (proto3 semantics); prost's default codegen already matches that for
    // scalars. `optional` fields in the .proto become `Option<T>` by default
    // in prost 0.14, matching the `?:`-style optionality baked into the TS
    // client's generated types.
    config.bytes(["."]);

    config
        .compile_protos(&["proto/agent.proto"], &["proto"])
        .expect("failed to compile crates/maho-ai/proto/agent.proto");
}
