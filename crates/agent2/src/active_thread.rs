/*!
Active thread global accessor extracted from the former embedded MCP server module.

This module provides a minimal, decoupled API for:
* Storing the currently "active" conversational `Thread` entity (if any).
* Reading that active thread elsewhere (e.g. memory / history tools, backends).

By separating this from the embedded MCP server, the internal assistant
memory and history functionality can continue to operate even if the
embedded server and its CLI/socket exposure are removed.

Integration points:
* Code that creates or focuses a conversation thread should call
  `set_active_thread(Some(thread_entity), cx)`.
* Code that closes or defocuses all threads may call
  `set_active_thread(None, cx)` to clear it.
* Consumers (e.g. `ThreadMemoryBackend`) call `active_thread(cx)` to
  obtain the current thread (if any).

Behavior:
* If no thread has been set yet, `active_thread(cx)` returns `None`.
* Setting the thread replaces any previous value.

No persistence is performed here; this is an in‑memory global only.
*/

use gpui::{App, Entity, Global, ReadGlobal, UpdateGlobal};
use log;

/// Wrapper global containing the active `Thread` entity (or none).
pub struct GlobalActiveThread(Option<Entity<crate::thread::Thread>>);

impl Global for GlobalActiveThread {}

impl GlobalActiveThread {
    /// Internal helper to install / replace the global.
    fn replace(thread: Option<Entity<crate::thread::Thread>>, cx: &mut App) {
        GlobalActiveThread::set_global(cx, GlobalActiveThread(thread));
    }

    /// Retrieve the active thread (if any).
    pub fn active_thread(cx: &App) -> Option<Entity<crate::thread::Thread>> {
        <GlobalActiveThread as ReadGlobal>::global(cx).0.clone()
    }
}

/// Set (or clear) the active thread.
///
/// Call this whenever the user focuses a different thread, creates a new thread,
/// or when all threads are closed.
pub fn set_active_thread(thread: Option<Entity<crate::thread::Thread>>, cx: &mut App) {
    if thread.is_some() {
        log::info!("active_thread: set_active_thread(Some(Thread))");
    } else {
        log::info!("active_thread: set_active_thread(None)");
    }
    GlobalActiveThread::replace(thread, cx);
}

/// Return the currently active thread entity, if one is set.
pub fn active_thread(cx: &App) -> Option<Entity<crate::thread::Thread>> {
    GlobalActiveThread::active_thread(cx)
}

/// Initialize the active thread global to `None`.
///
/// This should be invoked early during agent initialization (before any
/// component that might query the active thread).
pub fn init_active_thread(cx: &mut App) {
    // Avoid overwriting if already initialized (idempotent).
    if GlobalActiveThread::active_thread(cx).is_none() {
        log::info!("active_thread: initializing global with None");
        GlobalActiveThread::replace(None, cx);
    }
}
