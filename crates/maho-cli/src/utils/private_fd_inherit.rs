//! Port of `packages/coding-agent/src/utils/private-fd-inherit.ts` (senpi#4898).

/// Synchronous receipt boundary for clearing handle inheritance on Windows.
/// In Rust std, `OwnedFd` / `OwnedHandle` never enables `HANDLE_FLAG_INHERIT` by default;
/// callers inject an OS clearer when adopting a raw handle.
pub fn clear_private_fd_inherit_with(
    fd: u32,
    is_open: impl FnOnce(u32) -> bool,
    clear: impl FnOnce(u32) -> bool,
) -> bool {
    if !is_open(fd) {
        return false;
    }
    clear(fd)
}
