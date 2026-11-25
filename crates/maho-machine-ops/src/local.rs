//! Port of `packages/machine-ops/src/local.ts`.

use crate::files::glob::FdResolver;
use crate::files::local_files::LocalFileOps;
use crate::info::local_info::LocalInfoOps;
use crate::limits::DEFAULT_LIMITS;
use crate::policy::gate::gated;
use crate::process::exec::{KillTreeFn, default_kill_tree, run_exec};
use crate::process::handles::HandleRegistry;
use crate::process::holder_link::HolderLink;
use crate::process::scopes::ScopeRegistry;
use crate::types::{
    EditRequest, EditResult, ExecRequest, ExecResult, FileOps, GlobRequest, GlobResult,
    GrepRequest, GrepResult, HandleInfo, InfoOps, Limits, ListRequest, ListResult, MachineOps,
    OpContext, OpFuture, OpName, ProcessHandle, ProcessOps, ReadRequest, ReadResult, SpawnRequest,
    StatRequest, StatResult, WriteRequest, WriteResult,
};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct LocalMachineOpsOptions {
    /// Root of the host's machine state; setting it configures the scope registry.
    pub state_dir: Option<PathBuf>,
    /// Owner of spawned PTYs.
    pub holder: Arc<dyn HolderLink>,
    /// Host defaults; every op reads its own limits from `OpContext.limits`.
    pub limits: Option<Limits>,
    /// Process-tree kill for exec abort/timeout.
    pub kill_process_tree: Option<KillTreeFn>,
    /// `fd` lookup for `files.glob`.
    pub resolve_fd: Option<FdResolver>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

struct TouchClock {
    last_op_at_ms: AtomicU64,
}

impl TouchClock {
    fn new() -> Self {
        Self {
            last_op_at_ms: AtomicU64::new(0),
        }
    }

    fn touch(&self) {
        self.last_op_at_ms.store(now_ms().max(1), Ordering::Relaxed);
    }

    fn get(&self) -> Option<u64> {
        let val = self.last_op_at_ms.load(Ordering::Relaxed);
        if val == 0 { None } else { Some(val) }
    }
}

struct TouchedFileOps {
    inner: LocalFileOps,
    clock: Arc<TouchClock>,
}

impl FileOps for TouchedFileOps {
    fn read<'a>(&'a self, req: ReadRequest, ctx: &'a OpContext) -> OpFuture<'a, ReadResult> {
        self.clock.touch();
        self.inner.read(req, ctx)
    }
    fn write<'a>(&'a self, req: WriteRequest, ctx: &'a OpContext) -> OpFuture<'a, WriteResult> {
        self.clock.touch();
        self.inner.write(req, ctx)
    }
    fn edit<'a>(&'a self, req: EditRequest, ctx: &'a OpContext) -> OpFuture<'a, EditResult> {
        self.clock.touch();
        self.inner.edit(req, ctx)
    }
    fn glob<'a>(&'a self, req: GlobRequest, ctx: &'a OpContext) -> OpFuture<'a, GlobResult> {
        self.clock.touch();
        self.inner.glob(req, ctx)
    }
    fn grep<'a>(&'a self, req: GrepRequest, ctx: &'a OpContext) -> OpFuture<'a, GrepResult> {
        self.clock.touch();
        self.inner.grep(req, ctx)
    }
    fn stat<'a>(&'a self, req: StatRequest, ctx: &'a OpContext) -> OpFuture<'a, StatResult> {
        self.clock.touch();
        self.inner.stat(req, ctx)
    }
    fn list<'a>(&'a self, req: ListRequest, ctx: &'a OpContext) -> OpFuture<'a, ListResult> {
        self.clock.touch();
        self.inner.list(req, ctx)
    }
}

struct LocalProcessOps {
    scopes: Option<Arc<ScopeRegistry>>,
    handles: Arc<HandleRegistry>,
    kill_tree: KillTreeFn,
    clock: Arc<TouchClock>,
}

impl ProcessOps for LocalProcessOps {
    fn exec<'a>(&'a self, req: ExecRequest, ctx: &'a OpContext) -> OpFuture<'a, ExecResult> {
        self.clock.touch();
        Box::pin(async move {
            let args = json!({ "command": req.command });
            gated(OpName::ProcessExec, &args, ctx, || {
                run_exec(req, ctx, self.scopes.as_deref(), &self.kill_tree)
            })
            .await
        })
    }

    fn spawn<'a>(
        &'a self,
        req: SpawnRequest,
        ctx: &'a OpContext,
    ) -> OpFuture<'a, Arc<dyn ProcessHandle>> {
        self.clock.touch();
        Box::pin(async move {
            let args = json!({ "command": req.command });
            gated(OpName::ProcessSpawn, &args, ctx, || {
                self.handles.spawn(req, &ctx.principal.id, ctx.limits)
            })
            .await
        })
    }

    fn handle(&self, id: &str) -> Option<Arc<dyn ProcessHandle>> {
        self.handles.get(id)
    }

    fn list(&self, principal: Option<&str>) -> Vec<HandleInfo> {
        self.handles.list(principal)
    }

    fn sweep<'a>(&'a self, tokens: &'a [String]) -> OpFuture<'a, ()> {
        self.clock.touch();
        Box::pin(async move {
            if let Some(scopes) = &self.scopes {
                scopes.sweep(tokens).await;
            }
            Ok(())
        })
    }
}

pub struct LocalMachineOps {
    files: TouchedFileOps,
    process: LocalProcessOps,
    info: LocalInfoOps,
    limits: Limits,
}

impl MachineOps for LocalMachineOps {
    fn files(&self) -> &dyn FileOps {
        &self.files
    }

    fn process(&self) -> &dyn ProcessOps {
        &self.process
    }

    fn info(&self) -> &dyn InfoOps {
        &self.info
    }

    fn limits(&self) -> Limits {
        self.limits
    }
}

/// The local implementation of every op, one instance per host runtime.
pub fn create_local_machine_ops(options: LocalMachineOpsOptions) -> Arc<dyn MachineOps> {
    let limits = options.limits.unwrap_or(DEFAULT_LIMITS);
    let kill_tree: KillTreeFn = options
        .kill_process_tree
        .unwrap_or_else(|| Arc::new(default_kill_tree));
    let scopes = options
        .state_dir
        .as_ref()
        .map(|dir| Arc::new(ScopeRegistry::new(dir.clone(), kill_tree.clone(), None)));
    let handles = Arc::new(HandleRegistry::new(options.holder, scopes.clone(), limits));
    let clock = Arc::new(TouchClock::new());
    let handles_lookup = {
        let handles = handles.clone();
        Arc::new(move |id: &str| handles.get(id))
    };
    let last_op_lookup = {
        let clock = clock.clone();
        Arc::new(move || clock.get())
    };
    Arc::new(LocalMachineOps {
        files: TouchedFileOps {
            inner: LocalFileOps::new(options.resolve_fd),
            clock: clock.clone(),
        },
        process: LocalProcessOps {
            scopes,
            handles,
            kill_tree,
            clock,
        },
        info: LocalInfoOps::new(options.state_dir, handles_lookup, last_op_lookup),
        limits,
    })
}
