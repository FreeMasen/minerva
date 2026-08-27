//! Watch the library directory and keep the in-memory catalog in sync.
//!
//! A background thread receives filesystem events and, after coalescing a burst
//! (a file copy can emit many), rescans the directory and atomically swaps the
//! shared catalog. Adding an EPUB makes it appear; removing one drops it.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use notify::{RecursiveMode, Watcher};

use crate::AppState;
use crate::catalog::Catalog;

/// How long to wait for the event stream to go quiet before rescanning.
const DEBOUNCE: Duration = Duration::from_millis(300);

/// Spawn the watcher thread. Watching failures are logged but non-fatal: the
/// server keeps serving the catalog it scanned at startup.
pub fn spawn(dir: PathBuf, state: Arc<AppState>) {
    std::thread::spawn(move || {
        let (tx, rx) = mpsc::channel();

        let mut watcher = match notify::recommended_watcher(move |res| {
            let _ = tx.send(res);
        }) {
            Ok(watcher) => watcher,
            Err(err) => {
                tracing::error!(%err, "failed to create filesystem watcher");
                return;
            }
        };

        if let Err(err) = watcher.watch(&dir, RecursiveMode::NonRecursive) {
            tracing::error!(%err, dir = %dir.display(), "failed to watch library directory");
            return;
        }
        tracing::info!(dir = %dir.display(), "watching library directory for changes");

        // Block until an event arrives, then drain the tail of the burst before
        // doing a single rescan. `recv` returning Err means the watcher was
        // dropped, which only happens as the process shuts down.
        while rx.recv().is_ok() {
            while rx.recv_timeout(DEBOUNCE).is_ok() {}

            let catalog = Catalog::from_dir(&dir);
            let count = catalog.books().len();
            *state.catalog.write().unwrap() = Arc::new(catalog);
            tracing::info!(count, "library reloaded after filesystem change");
        }

        // Keep the watcher alive for the lifetime of the loop.
        drop(watcher);
    });
}
