# Plan / deferred work

Tracking things intentionally left for later so they don't get lost.

## Deferred polish (agreed to wait)

- **Watcher scalability.** The `notify` watcher is non-recursive and does a full
  directory rescan on every change (debounced). Fine for modest libraries; a
  large library would want recursive watching and incremental add/remove updates
  instead of a full rescan. (`src/watch.rs`, `Catalog::from_dir`)
- **Thumbnail resizing.** `/opds/covers/{id}/thumb` currently serves the same
  embedded image as the full cover (no server-side resize). Real thumbnails
  would need an image-processing crate. (`src/main.rs` `serve_cover`)
- **Category heuristic.** File-backed books get their Fiction/Non-Fiction
  category from a crude keyword scan of the EPUB subjects, defaulting to
  Non-Fiction. Could be smarter, configurable, or a richer taxonomy.
  (`Category::classify` in `src/catalog.rs`)

## Out of OPDS 2.0 core (separate specs / extensions)

These are NOT part of the OPDS 2.0 document itself; pick up only if wanted.

- **Authentication for OPDS** — `application/opds-authentication+json`, the 401
  challenge + login flow. Defined by a separate spec.
- **Library availability** — `availability` / `holds` / `copies` properties for
  lending. An OPDS extension, not in core 2.0.
