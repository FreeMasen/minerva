//! Watch the library directory and keep the SQLite catalog in sync.
//!
//! `notify-debouncer-full` runs the filesystem backend on its own thread,
//! coalesces bursts of events, and (after a quiet period) invokes a synchronous
//! callback with the settled set of changes. That callback only forwards the
//! result over a channel; an async task owns the receiver and the catalog store
//! and does the real work, so there is no blocking bridge back into the runtime.

/// How long the event stream must be quiet before a batch is delivered.
const DEBOUNCE: Duration = Duration::from_millis(500);

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify::event::ModifyKind;
use notify::{EventKind, RecursiveMode, Watcher};
use notify_debouncer_full::{DebounceEventResult, DebouncedEvent, new_debouncer};
use tokio::sync::mpsc::{self, UnboundedReceiver};

use crate::catalog::{self, book_from_file};
use crate::library::CatalogStore;

/// Start watching `dir`, updating `store` as EPUBs are added, changed or
/// removed. Returns an error if the watch can't be established.
pub fn spawn(dir: PathBuf, store: Arc<CatalogStore>) -> anyhow::Result<()> {
    let (tx, rx) = mpsc::unbounded_channel();

    // The debouncer callback runs on its own thread; just forward the batch.
    let mut debouncer = new_debouncer(DEBOUNCE, None, move |result| {
        let _ = tx.send(result);
    })?;
    debouncer
        .watcher()
        .watch(&dir, RecursiveMode::Recursive)?;
    // Seed the rename-tracking cache so directory renames are reported.
    debouncer.cache().add_root(&dir, RecursiveMode::Recursive);
    tracing::info!(dir = %dir.display(), "watching library directory for changes");

    // The async task owns the debouncer (keeping it alive), the receiver and the
    // store, and reacts to settled batches without leaving the runtime.
    tokio::spawn(async move {
        let _debouncer = debouncer;
        process_events(dir, store, rx).await;
    });
    Ok(())
}

/// Apply each settled batch of events to the store.
async fn process_events(
    dir: PathBuf,
    store: Arc<CatalogStore>,
    mut rx: UnboundedReceiver<DebounceEventResult>,
) {
    while let Some(result) = rx.recv().await {
        let events = match result {
            Ok(events) => events,
            Err(errors) => {
                for error in errors {
                    tracing::warn!(?error, "filesystem watch error");
                }
                // Errors (e.g. a dropped/overflowed backend queue) may mean we
                // missed changes; reconcile to be safe.
                store.reconcile_dir(&dir).await;
                continue;
            }
        };

        if apply_batch(&dir, &store, events).await {
            let count = store.count().await;
            tracing::info!(count, "catalog updated after filesystem change");
        }
    }
}

/// Apply a settled batch of events to the store. Returns whether anything
/// changed. EPUB paths are applied surgically; a *structural* directory change
/// (create/remove/rename) triggers a full reconcile, while a directory merely
/// being modified because a child changed is ignored.
async fn apply_batch(dir: &Path, store: &CatalogStore, events: Vec<DebouncedEvent>) -> bool {
    let mut affected: HashSet<PathBuf> = HashSet::new();
    let mut full_reconcile = false;

    for event in events {
        let structural = matches!(
            event.kind,
            EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_))
        );
        for path in &event.paths {
            if catalog::is_epub(path) {
                affected.insert(path.clone());
            } else if structural
                && (path.is_dir() || (!path.exists() && store.has_books_under(path).await))
            {
                // A directory that (now) exists was created/renamed, or one that
                // held books was removed.
                full_reconcile = true;
            }
            // Otherwise a non-EPUB file, or a directory merely modified: ignore.
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
                    let mtime = file_mtime(&path);
                    let (book, category) = book_from_file(dir, path, meta);
                    store.upsert_file(&book, mtime, &category).await;
                }
                Err(err) => {
                    tracing::warn!(?err, path = %path.display(), "dropping unreadable EPUB");
                    store.delete_by_path(&path).await;
                }
            }
        } else {
            store.delete_by_path(&path).await;
        }
    }

    true
}

/// A file's modification time as Unix seconds.
fn file_mtime(path: &Path) -> Option<i64> {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
}
