use std::sync::OnceLock;

use anyhow::{Context, anyhow};
use tokio::runtime::{Handle, Runtime};

use anyhow::Result;

/// Global Tokio runtime used for database and other async operations that require
/// a stable Tokio context to allow internal maintenance tasks (e.g. sqlx pool
/// background workers) to keep running for the lifetime of the application.
///
/// Reason: Creating an ephemeral runtime just to `block_on` a connect call drops
/// the scheduler immediately afterward, causing spawned maintenance tasks to
/// panic when they attempt to use a missing Tokio context.
static GLOBAL_RUNTIME: OnceLock<Runtime> = OnceLock::new();

/// Initialize (if needed) and return a handle to the global runtime.
///
/// This returns a `Result` so callers can handle initialization failure instead
/// of panicking. Subsequent calls are cheap.
///
/// The runtime is multi-thread and `enable_all` so that timers, IO, and
/// other Tokio facilities are available to dependencies.
pub fn ensure_global_tokio_runtime() -> Result<&'static Handle> {
    let runtime = if let Some(rt) = GLOBAL_RUNTIME.get() {
        rt
    } else {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("failed to build global Tokio runtime")?;
        GLOBAL_RUNTIME
            .set(rt)
            .map_err(|_| anyhow!("global Tokio runtime already initialized"))?;
        GLOBAL_RUNTIME.get().unwrap()
    };

    Ok(runtime.handle())
}

/// Spawn a future onto the global runtime.
///
/// Returns an error if the runtime could not be initialized.
pub fn spawn_global<F>(future: F) -> Result<tokio::task::JoinHandle<F::Output>>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    let handle = ensure_global_tokio_runtime()?;
    Ok(handle.spawn(future))
}

/// Convenience for running a future to completion on the global runtime.
///
/// Avoid using this for long‑running or blocking operations inside the foreground
/// thread; prefer `spawn_global` and await in an async context if possible.
pub fn block_on_global<F, T>(future: F) -> Result<T>
where
    F: std::future::Future<Output = T> + Send,
    T: Send,
{
    let runtime = if let Some(rt) = GLOBAL_RUNTIME.get() {
        rt
    } else {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("failed to build global Tokio runtime")?;
        GLOBAL_RUNTIME
            .set(rt)
            .map_err(|_| anyhow!("global Tokio runtime already initialized"))?;
        GLOBAL_RUNTIME.get().unwrap()
    };

    // `Runtime::block_on` can panic if called while another call is active on
    // the same multi-thread runtime in a nested fashion. Guard against nested
    // entry by checking if a Tokio context is already present.
    if Handle::try_current().is_ok() {
        // Nested block_on would deadlock or panic; surface a controlled error.
        return Err(anyhow!(
            "block_on_global called from within an existing Tokio runtime context"
        ));
    }

    Ok(runtime.block_on(future))
}

/// Returns true if the global runtime has already been initialized.
pub fn global_runtime_initialized() -> bool {
    GLOBAL_RUNTIME.get().is_some()
}

/// Run a future on the global Tokio runtime and return its output.
///
/// This helper is synchronous. It must not be called from within an existing Tokio
/// runtime context because it relies on `block_on_global` internally, which guards
/// against nested runtime entry. If you are already inside an async context, prefer
/// `ensure_global_tokio_runtime()` and `Handle::spawn` / `.await` directly.
///
/// Errors:
/// * Returns an error if invoked while a Tokio runtime is already active on the current thread.
/// * Propagates join errors and converts them to anyhow::Error.
pub fn run_on_global<F, T>(future: F) -> Result<T>
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    if Handle::try_current().is_ok() {
        return Err(anyhow!(
            "run_on_global cannot be called from within an existing Tokio runtime context"
        ));
    }

    let handle = ensure_global_tokio_runtime()?;
    let join = handle.spawn(future);
    // Use block_on_global to drive the spawned future to completion outside the runtime context.
    block_on_global(async { join.await.map_err(|e| anyhow!(e)) })?
}

/// Enter the global Tokio runtime on the current thread, returning a guard
/// that exits the runtime when dropped. This is useful for code that needs
/// to call APIs that assume they are running inside a Tokio context (e.g.
/// sqlx / sea-orm pool initialization spawning maintenance tasks).
///
/// If the runtime is not yet initialized, it will be created.
/// This should be called as early as possible in threads that will
/// perform database connections.
///
/// Returns:
/// * `Ok(Some(guard))` if the thread successfully entered the runtime.
/// * `Ok(None)` if already inside a Tokio runtime (nested enter avoided).
/// * `Err(anyhow::Error)` if initialization failed.
pub fn enter_global_runtime() -> Result<Option<tokio::runtime::EnterGuard<'static>>> {
    match Handle::try_current() {
        Ok(_) => Ok(None),
        Err(_) => {
            let handle = ensure_global_tokio_runtime()?;
            Ok(Some(handle.enter()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_initializes_once() {
        assert!(!global_runtime_initialized());
        let handle_a = ensure_global_tokio_runtime().expect("init runtime");
        assert!(global_runtime_initialized());
        let handle_b = ensure_global_tokio_runtime().expect("reuse runtime");
        assert!(
            std::ptr::eq(handle_a, handle_b),
            "handles should be identical"
        );
    }

    #[test]
    fn spawn_and_block_on() {
        let handle = ensure_global_tokio_runtime().expect("runtime");
        let join = spawn_global(async { 42 }).expect("spawn");
        let result = handle.block_on(join).expect("join");
        assert_eq!(result, 42);
    }

    #[test]
    fn block_on_global_errors_inside_context() {
        let handle = ensure_global_tokio_runtime().expect("runtime");
        let err = handle.block_on(async {
            block_on_global(async { 1 }).expect_err("should error inside runtime")
        });
        assert!(
            err.to_string()
                .contains("block_on_global called from within an existing Tokio runtime"),
            "unexpected error message: {err}"
        );
    }
}
