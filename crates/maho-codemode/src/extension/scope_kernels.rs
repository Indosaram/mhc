use super::session_manager::{CodemodeSessionManager, CreateCodemodeSessionManagerOptions};
use super::session_manager_proxy::SessionProxyError;
use crate::bridge::protocol::LocalRoots;
use crate::kernels::session_env::InheritedEnvironment;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

/// One execution scope (`ExecutionScope`): an opaque id minted by the loop, and its root
/// directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KernelScope {
    pub id: String,
    pub root: String,
}

/// What belongs to one scope and cannot be shared from the runtime's options.
#[derive(Clone, Default)]
pub struct KernelScopeOwned {
    pub session_env: Option<crate::kernels::session_env::SessionEnvironment>,
    pub inherited_env: Option<InheritedEnvironment>,
}

pub type ScopeManagerFuture =
    Pin<Box<dyn Future<Output = Result<Arc<CodemodeSessionManager>, String>> + Send>>;
pub type ScopeManagerFactory =
    Arc<dyn Fn(CreateCodemodeSessionManagerOptions) -> ScopeManagerFuture + Send + Sync>;

fn scope_directory(id: &str) -> String {
    let digest = Sha256::digest(id.as_bytes());
    let mut hex = String::with_capacity(24);
    for byte in &digest[..12] {
        use std::fmt::Write;
        let _ = write!(&mut hex, "{byte:02x}");
    }
    hex
}

fn scope_options(
    base: &CreateCodemodeSessionManagerOptions,
    scope: &KernelScope,
    owned: KernelScopeOwned,
) -> CreateCodemodeSessionManagerOptions {
    let directory = scope_directory(&scope.id);
    let in_scope = |path: &Path| path.join("scopes").join(&directory);
    let artifacts_dir = base.artifacts_dir.as_deref().map(in_scope);
    let local_roots = base.local_roots.as_ref().map(|roots| {
        LocalRoots(
            roots
                .0
                .iter()
                .map(|(name, root)| {
                    (
                        name.clone(),
                        in_scope(Path::new(root)).to_string_lossy().into_owned(),
                    )
                })
                .collect(),
        )
    });
    let mut options = base.clone();
    options.session_id = scope.id.clone();
    options.owner_session_id = Some(scope.id.clone());
    if !scope.root.is_empty() {
        options.cwd = Path::new(&scope.root).to_path_buf();
    }
    options.artifacts_dir = artifacts_dir;
    options.local_roots = local_roots;
    options.session_env = owned.session_env;
    if owned.inherited_env.is_some() {
        options.inherited_env = owned.inherited_env;
    }
    options
}

type SharedPending = Arc<tokio::sync::OnceCell<Result<Arc<CodemodeSessionManager>, String>>>;

struct ScopedRegistryInner {
    disposed: bool,
    managers: HashMap<String, SharedPending>,
}

/// Kernels per execution scope (brain/hand S16b): when one runtime serves several scopes, each gets
/// its own session manager, so its kernels, globals, bridge, completion context, ledgers and
/// `local://` root are its own. Releasing a scope closes exactly its kernels; the others keep
/// running.
pub struct ScopedKernelManagers {
    base: CreateCodemodeSessionManagerOptions,
    create: ScopeManagerFactory,
    inner: Arc<Mutex<ScopedRegistryInner>>,
}

impl ScopedKernelManagers {
    pub fn new(base: CreateCodemodeSessionManagerOptions, create: ScopeManagerFactory) -> Self {
        Self {
            base,
            create,
            inner: Arc::new(Mutex::new(ScopedRegistryInner {
                disposed: false,
                managers: HashMap::new(),
            })),
        }
    }

    pub fn with_default_factory(base: CreateCodemodeSessionManagerOptions) -> Self {
        Self::new(
            base,
            Arc::new(|options| {
                Box::pin(async move {
                    CodemodeSessionManager::start(options)
                        .await
                        .map(Arc::new)
                        .map_err(|error| error.to_string())
                })
            }),
        )
    }

    pub async fn for_scope(
        &self,
        scope: &KernelScope,
        owned: KernelScopeOwned,
    ) -> Result<Arc<CodemodeSessionManager>, String> {
        let cell = {
            let mut guard = self.inner.lock().expect("scoped kernel managers lock");
            if guard.disposed {
                return Err(SessionProxyError::Disposed.to_string());
            }
            if let Some(existing) = guard.managers.get(&scope.id) {
                Arc::clone(existing)
            } else {
                let cell = Arc::new(tokio::sync::OnceCell::new());
                guard.managers.insert(scope.id.clone(), Arc::clone(&cell));
                cell
            }
        };
        let options = scope_options(&self.base, scope, owned);
        let create = Arc::clone(&self.create);
        let result = cell
            .get_or_init(|| async move { (create)(options).await })
            .await
            .clone();
        if result.is_err() {
            let mut guard = self.inner.lock().expect("scoped kernel managers lock");
            if guard
                .managers
                .get(&scope.id)
                .is_some_and(|existing| Arc::ptr_eq(existing, &cell))
            {
                guard.managers.remove(&scope.id);
            }
        }
        result
    }

    pub async fn release_scope(&self, id: &str) -> Result<(), String> {
        let cell = {
            self.inner
                .lock()
                .expect("scoped kernel managers lock")
                .managers
                .remove(id)
        };
        if let Some(cell) = cell
            && let Some(Ok(manager)) = cell.get()
        {
            manager.dispose().await?;
        }
        Ok(())
    }

    pub async fn dispose(&self) -> Result<(), String> {
        let entries: Vec<SharedPending> = {
            let mut guard = self.inner.lock().expect("scoped kernel managers lock");
            guard.disposed = true;
            guard.managers.drain().map(|(_, cell)| cell).collect()
        };
        let mut failures = Vec::new();
        for cell in entries {
            if let Some(Ok(manager)) = cell.get()
                && let Err(error) = manager.dispose().await
            {
                failures.push(error);
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "Failed to dispose scoped codemode kernels: {}",
                failures.join("; ")
            ))
        }
    }
}
