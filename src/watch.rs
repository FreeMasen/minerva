//! Watch the library directory and keep the in-memory catalog in sync.
//!
//! A background thread receives filesystem events and, after coalescing a burst
//! (a file copy can emit many), updates a path-keyed index and atomically swaps
//! the shared catalog. File-level changes are applied incrementally — only the
//! affected EPUBs are re-read — while directory-level changes fall back to a
//! full recursive rescan for correctness.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use notify::{RecursiveMode, Watcher};

use crate::AppState;
use crate::catalog::{self, Book, Catalog};

/// How long to wait for the event stream to go quiet before reacting.
const DEBOUNCE: Duration = Duration::from_millis(300);

/// Spawn the watcher thread over `dir`, seeded with `index` (the initial scan).
/// Watching failures are logged but non-fatal: the server keeps serving the
/// catalog it scanned at startup.
pub fn spawn(dir: PathBuf, index: HashMap<PathBuf, Book>, state: Arc<AppState>) {
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

        // Recursive so books in subdirectories are tracked too.
        if let Err(err) = watcher.watch(&dir, RecursiveMode::Recursive) {
            tracing::error!(%err, dir = %dir.display(), "failed to watch library directory");
            return;
        }
        tracing::info!(dir = %dir.display(), "watching library directory for changes");

        let mut index = index;

        // Block until an event arrives, then drain the tail of the burst before
        // reacting. `recv` returning Err means the watcher was dropped, which
        // only happens as the process shuts down.
        while let Ok(first) = rx.recv() {
            let mut batch = vec![first];
            while let Ok(event) = rx.recv_timeout(DEBOUNCE) {
                batch.push(event);
            }

            if apply_batch(&dir, &mut index, batch) {
                let catalog = Catalog::from_index(&index);
                let count = catalog.books().len();
                *state.catalog.write().unwrap() = Arc::new(catalog);
                tracing::info!(count, "library reloaded after filesystem change");
            }
        }

        // Keep the watcher alive for the lifetime of the loop.
        drop(watcher);
    });
}

/// Apply a coalesced batch of events to `index`. Returns whether anything
/// changed (and the catalog should be rebuilt).
///
/// EPUB file paths are handled incrementally: re-read on create/modify, dropped
/// when gone. Anything else (a directory added, renamed or removed) triggers a
/// full rescan, which is always correct if less surgical.
fn apply_batch(
    dir: &PathBuf,
    index: &mut HashMap<PathBuf, Book>,
    batch: Vec<Result<notify::Event, notify::Error>>,
) -> bool {
    let mut affected: HashSet<PathBuf> = HashSet::new();
    let mut full_rescan = false;

    for result in batch {
        let event = match result {
            Ok(event) => event,
            // A dropped/overflowed event stream: rescan to be safe.
            Err(_) => {
                full_rescan = true;
                continue;
            }
        };
        for path in event.paths {
            if catalog::is_epub(&path) {
                affected.insert(path);
            } else if path.is_dir() {
                // A directory appeared or was renamed into place.
                full_rescan = true;
            } else if !path.exists() && index.keys().any(|k| k.starts_with(&path)) {
                // A directory that held books was removed.
                full_rescan = true;
            }
            // Otherwise a non-EPUB file (e.g. .DS_Store): ignore.
        }
    }

    if full_rescan {
        *index = catalog::scan(dir);
        return true;
    }

    if affected.is_empty() {
        return false;
    }

    for path in affected {
        if path.is_file() {
            match crate::epub::read_meta(&path) {
                Ok(meta) => {
                    index.insert(path.clone(), catalog::book_from_file(dir, path, meta));
                }
                Err(err) => {
                    // Treat an unreadable/half-written file as absent for now; a
                    // later event once the write settles will pick it up.
                    tracing::warn!(%err, path = %path.display(), "dropping unreadable EPUB");
                    index.remove(&path);
                }
            }
        } else {
            index.remove(&path);
        }
    }

    true
}
