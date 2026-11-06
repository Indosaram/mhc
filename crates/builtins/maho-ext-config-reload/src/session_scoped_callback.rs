use maho_ai::node::provider_scope::{
    ProviderScopeError, ScopeState, active_provider_scope, run_with_provider_scope,
};

pub fn bind_session_scoped_callback<T>(
    callback: impl Fn(T) + Send + Sync + 'static,
) -> impl Fn(T) -> Result<(), ProviderScopeError> {
    let scope = active_provider_scope();
    move |args| match &scope {
        None => {
            callback(args);
            Ok(())
        }
        Some(scope) if scope.state() == ScopeState::Closed => Ok(()),
        Some(scope) => run_with_provider_scope(scope, || callback(args)),
    }
}
