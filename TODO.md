# Plan / deferred work

## Done (recent batch)

- **Fewer allocations building the wire model** — the constant-bearing link/
  metadata fields (`Link::rel`/`type`/`title`, `Metadata::@type`, `Price`,
  `Availability::state`, `IndirectAcquisition`, auth flow type) are now
  `Cow<'static, str>`. String *constants* (rels, media types) serialize as
  zero-alloc borrows instead of allocating a fresh `String` each time, while
  dynamic values (a file's media type, category labels) still coerce in as
  `Cow::Owned`. Per publication that's roughly 21 -> 13 heap allocations in the
  placeholder-cover path (more in the borrow/buy paths); every navigation,
  facet, and pagination link in a feed drops its two constant allocations too.
  Output is byte-identical (serde serializes `Cow<str>` as the string).
  (`src/model.rs`, `src/catalog.rs`)

- **Multi-format books (XTC/XTCH)** — a book is a logical work (`books`, grouped
  by `work_key` = title + author) backed by one or more format files
  (`book_files`). Scanning reads `.epub`/`.xtc`/`.xtch`; files matching on
  title+author merge into one publication with one `open-access` link per format
  (`/opds/download/{id}/{format}`), and the richest format (`meta_rank`) supplies
  the shared metadata. (`src/xtc.rs`, `migrations/0003_book_files.sql`)

- **Demo scaffolding is test-only** — EPUB generation (`assets`) and the sample
  catalog (`sample_books`/`reset_to_samples`) compile for tests only. Runtime
  cover generation moved to `src/covers.rs`. A library directory
  (`OPDS_LIBRARY_DIR`) is now required to run the server.

- **walkdir** — `catalog::epub_paths` uses walkdir instead of a hand-rolled walk.
- **jiff timestamps** — `Book::modified` is a `jiff::Timestamp`, parsed/formatted
  at the DB, EPUB, and wire boundaries.
- **Authors as categories** — a "Browse by Author" group and `/opds/authors/{slug}`
  feeds, derived from the author column.
- **Server admin** — CLI subcommands (set-title/set-author/add-category/
  remove-category/remove-book) and a tera-templated web UI at `/admin`
  (edit properties, add/remove categories, remove book, upload EPUB).
- **base64 crate** — replaced the hand-rolled base64 module.
- **Data-driven categories** — many-to-many `categories`/`book_categories`
  tables with assign/remove endpoints.

## Done

- **Watcher scalability** — scanning is recursive and the watcher updates
  incrementally (only changed EPUBs are re-read), falling back to a full rescan
  for directory-level changes. (`src/watch.rs`, `src/catalog.rs`)
- **Thumbnail resizing** — `/opds/covers/{id}/thumb` downscales the embedded
  cover to fit 160x240 and re-encodes as JPEG. (`assets::thumbnail`)
- **Category heuristic** — categories prefer a top-level `Fiction/` or
  `Non-Fiction/` library subfolder, with a broadened subject fallback.
  (`Category::from_path` / `classify`)
- **Authentication for OPDS** — optional multi-account HTTP Basic auth backed
  by a SQLite user store (`OPDS_AUTH_DB`) with Argon2-hashed passwords and
  constant-time verification (RustCrypto); accounts managed via `adduser`. A
  401 challenge returns an `application/opds-authentication+json` document,
  served publicly at `/opds/auth`. (`src/auth.rs`, `src/main.rs`, `src/base64.rs`)
- **Library availability** — `availability` / `holds` / `copies` on `borrow`
  acquisition links (an OPDS extension). (`src/model.rs`, `src/catalog.rs`)

## Possible future work (not requested)

- Real purchase/borrow flows (currently 501) with entitlement + gated delivery.
- Token/OAuth auth flows; per-user entitlements; roles/admin.
- Larger-library performance: stream downloads instead of buffering; cache
  extracted covers.

Data access now uses sqlx with a connection pool and compile-time-checked
queries, so the earlier "SQLite connection pool" item is done.
