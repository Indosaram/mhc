//! The production tool child's role (senpi `modes/rpc/tool-child/role.ts`): which job kinds it serves, then the
//! child's entry loop. It lives in maho-cli because it needs the terminal and user-bash handlers from crates above
//! `maho-tool-child`.
use std::collections::BTreeMap;

use maho_tool_child::entry::{ToolChildEntryOptions, ToolChildJobHandler, run_tool_child};
use maho_tool_child::module_job::{MODULE_JOB_KIND, create_module_job_handler};
use maho_tool_child::user_bash_job::{USER_BASH_JOB_KIND, create_user_bash_job_handler};

/// Job kinds the production tool child serves; each later migration registers its kind here.
pub fn tool_child_job_handlers() -> BTreeMap<String, ToolChildJobHandler> {
    let mut handlers = BTreeMap::new();
    handlers.insert(
        MODULE_JOB_KIND.to_owned(),
        create_module_job_handler(maho_tool_child::launch_env_names::process_env()),
    );
    handlers.extend(maho_ext_terminal::terminal_jobs::create_terminal_job_handlers(None));
    handlers.insert(
        USER_BASH_JOB_KIND.to_owned(),
        create_user_bash_job_handler(),
    );
    handlers
}

/// `runToolChildRole`: serves the handlers until the channel closes, then exits 0.
pub fn run_tool_child_role() -> ! {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("tool-child: {error}");
            std::process::exit(crate::internal_role::INTERNAL_ROLE_EXIT);
        }
    };
    let served = runtime.block_on(run_tool_child(ToolChildEntryOptions {
        handlers: tool_child_job_handlers(),
        env: None,
        exit: None,
    }));
    if let Err(error) = served {
        // senpi lets the rejection escape `runToolChildRole()` (an unhandled rejection exits non-zero).
        eprintln!("tool-child: {error}");
        std::process::exit(1);
    }
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serves_the_module_terminal_and_user_bash_kinds() {
        let handlers = tool_child_job_handlers();
        assert!(handlers.contains_key(MODULE_JOB_KIND));
        assert!(handlers.contains_key(USER_BASH_JOB_KIND));
        assert!(
            handlers.len() >= 3,
            "terminal job kinds are registered beside module and user.bash"
        );
    }
}
