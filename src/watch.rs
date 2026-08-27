//! Watch the library directory and keep the SQLite catalog in sync.
//!
//! A background thread receives filesystem events and, after coalescing a burst
//! (a file copy can emit many), updates the catalog store. The store is async,
//! so the thread drives it with a runtime handle. File-level changes are applied
//! surgically — the affected EPUB is re-read and upserted, or its row deleted —
//! while directory-level changes trigger a full reconcile.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use notify::{RecursiveMode, Watcher};
use tokio::runtime::Handle;

use crate::catalog::{self, book_from_file};
use crate::library::CatalogStore;

/// How long to wait for the event stream to go quiet before reacting.
const DEBOUNCE: Duration = Duration::from_millis(300);

/// Spawn the watcher thread over `dir`, updating `store` (via `handle`) as files
/// change. Watching failures are logged but non-fatal: the server keeps serving
/// the catalog reconciled at startup.
pub fn spawn(dir: PathBuf, store: Arc<CatalogStore>, handle: Handle) {
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

        if let Err(err) = watcher.watch(&dir, RecursiveMode::Recursive) {
            tracing::error!(%err, dir = %dir.display(), "failed to watch library directory");
            return;
        }
        tracing::info!(dir = %dir.display(), "watching library directory for changes");

        while let Ok(first) = rx.recv() {
            let mut batch = vec![first];
            while let Ok(event) = rx.recv_timeout(DEBOUNCE) {
                batch.push(event);
            }

            if handle.block_on(apply_batch(&dir, &store, batch)) {
                let count = handle.block_on(store.count());
                tracing::info!(count, "catalog updated after filesystem change");
            }
        }

        // Keep the watcher alive for the lifetime of the loop.
        drop(watcher);
    });
}

/// Apply a coalesced batch of events to the store. Returns whether anything
/// changed.
async fn apply_batch(
    dir: &PathBuf,
    store: &CatalogStore,
    batch: Vec<Result<notify::Event, notify::Error>>,
) -> bool {
    let mut affected: HashSet<PathBuf> = HashSet::new();
    let mut full_reconcile = false;

    for result in batch {
        let event = match result {
            Ok(event) => event,
            Err(_) => {
                full_reconcile = true;
                continue;
            }
        };
        for path in event.paths {
            if catalog::is_epub(&path) {
                affected.insert(path);
            } else if path.is_dir() {
                // A directory appeared or was renamed into place.
                full_reconcile = true;
            } else if !path.exists() && store.has_books_under(&path).await {
                // A directory that held books was removed.
                full_reconcile = true;
            }
            // Otherwise a non-EPUB file (e.g. .DS_Store): ignore.
        }
    }

    if full_reconcile {
        store.reconcile_dir(dir).await;
        return true;
    }

    if affected.is_empty() {
        return false;
    }

    for path in affected {
        if path.is_file() {
            match crate::epub::read_meta(&path) {
                Ok(meta) => {
                    let mtime = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64);
                    store.upsert_file(&book_from_file(dir, path, meta), mtime).await;
                }
                Err(err) => {
                    // Treat an unreadable/half-written file as absent for now; a
                    // later event once the write settles will re-add it.
                    tracing::warn!(%err, path = %path.display(), "dropping unreadable EPUB");
                    store.delete_by_path(&path).await;
                }
            }
        } else {
            store.delete_by_path(&path).await;
        }
    }

    true
}
