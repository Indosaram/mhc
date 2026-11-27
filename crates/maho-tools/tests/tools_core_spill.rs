use crate::tools_core_fake_machine::{ErrorMode, FakeMachine};
use maho_machine_ops::{MachineOpError, OpErrorCode};
use maho_tools::machine::create_tool_machine;
use maho_tools::machine_output_spill::create_machine_output_spill;
use maho_tools::output_accumulator_core::{CoreOutputAccumulator, CoreOutputAccumulatorOptions};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

#[tokio::test]
async fn closes_with_write_error_alone_when_no_chunk_reached_machine() {
    let fake = Arc::new(
        FakeMachine::new(HashMap::new(), ErrorMode::Fs, "linux").with_write_override(|_| {
            Err(MachineOpError::with_fs_code(
                OpErrorCode::IoError,
                "EROFS: read-only file system",
                "EROFS",
            ))
        }),
    );
    let machine = create_tool_machine(Some(fake), None);
    let mut output = CoreOutputAccumulator::new(
        CoreOutputAccumulatorOptions {
            max_bytes: 1,
            ..Default::default()
        },
        create_machine_output_spill(machine, Some(PathBuf::from("/nowhere"))),
    );
    output.append(b"too large").expect("append");
    output.finish().expect("finish");

    let err = output
        .close_temp_file()
        .await
        .expect_err("close fails with write error");
    assert!(
        err.to_string().contains("read-only file system"),
        "expected write error alone, got: {err}"
    );
    assert!(
        !err.to_string().contains("cleanup failed"),
        "unexpected cleanup wrapper error: {err}"
    );
}
