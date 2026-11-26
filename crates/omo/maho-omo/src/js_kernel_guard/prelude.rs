use maho_ext_api::KernelPrelude;

pub(super) fn js_kernel_guard_prelude() -> KernelPrelude {
    KernelPrelude {
        javascript: r#"globalThis["omoJsKernelGuard"] = true
process.exit = function omoCellProcessExit(code) {
  const error = new Error(
    "process.exit(" + String(code ?? 0) + ") ran inside a JavaScript cell. The kernel stayed up and its globals were kept."
  )
  error.name = "ProcessExitInCell"
  throw error
}"#
        .into(),
        python: "omoJsKernelGuard = True\n".into(),
        documentation: format!(
            "{} Calling process.exit inside a JS cell throws ProcessExitInCell and leaves the kernel and its globals in place.",
            super::CELL_HINT
        ),
        exports: vec!["omoJsKernelGuard".into()],
        ..Default::default()
    }
}
