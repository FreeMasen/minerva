//! Watch the library directory and keep the SQLite catalog in sync.
//!
//! `notify` runs its filesystem backend on its own thread and invokes a
//! synchronous callback there. That callback does nothing but forward events
//! over a channel; an async task owns the receiver (and the catalog store) and
//! does the real work, so there is no blocking bridge back into the runtime.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify::{RecursiveMode, Watcher};
use tokio::sync::mpsc::{self, UnboundedReceiver};

use crate::catalog::{self, book_from_file};
use crate::library::CatalogStore;

/// A filesystem event as delivered by `notify`.
type Event = Result<notify::Event, notify::Error>;

/// How long to wait for the event stream to go quiet before reacting.
const DEBOUNCE: Duration = Duration::from_millis(300);

/// Start watching `dir`, updating `store` as EPUBs are added, changed or
/// removed. Returns an error if the watch can't be established.
pub fn spawn(dir: PathBuf, store: Arc<CatalogStore>) -> anyhow::Result<()> {
    let (tx, rx) = mpsc::unbounded_channel();

    // The notify callback runs on notify's own thread; just forward events.
    let mut watcher = notify::recommended_watcher(move |event| {
        let _ = tx.send(event);
    })?;
    watcher.watch(&dir, RecursiveMode::Recursive)?;
    tracing::info!(dir = %dir.display(), "watching library directory for changes");

    // The async task owns the watcher (keeping it alive), the receiver and the
    // store, and reacts to events without leaving the runtime.
    tokio::spawn(async move {
        let _watcher = watcher;
        process_events(dir, store, rx).await;
    });
    Ok(())
}

/// Debounce bursts of events and apply each settled batch to the store.
async fn process_events(dir: PathBuf, store: Arc<CatalogStore>, mut rx: UnboundedReceiver<Event>) {
    while let Some(first) = rx.recv().await {
        // Coalesce a burst (a file copy emits many events) before reacting.
        let mut batch = vec![first];
        loop {
            match tokio::time::timeout(DEBOUNCE, rx.recv()).await {
                Ok(Some(event)) => batch.push(event),
                // Quiet period reached (Err) or channel closed (None).
                Ok(None) | Err(_) => break,
            }
        }

        if apply_batch(&dir, &store, batch).await {
            let count = store.count().await;
            tracing::info!(count, "catalog updated after filesystem change");
        }
    }
}

/// Apply a coalesced batch of events to the store. Returns whether anything
/// changed. File-level changes are applied surgically; directory-level changes
/// trigger a full reconcile.
async fn apply_batch(dir: &Path, store: &CatalogStore, batch: Vec<Event>) -> bool {
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
                    store
                        .upsert_file(&book_from_file(dir, path, meta), mtime)
                        .await;
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
