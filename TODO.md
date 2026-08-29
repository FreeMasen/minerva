# Plan / deferred work

## IN PROGRESS — "stringly-typed -> richer types" refactor (RESUME HERE)

Converting stringly-typed values into domain types. Four pieces were approved
(all of them). DB migrations are fine — nothing is deployed. Last clean commit
is `d95f04a` (the Cow refactor); everything below is uncommitted work on top.

Money decision: use a `UsdCents(u32)` newtype, NOT the `doubloon` crate.
doubloon is built on `rust_decimal` (no clean SQLite mapping — SQLite has no
decimal type) and its serde emits the amount as a *string*, whereas the OPDS
wire wants `price.value` as a JSON *number*. Prices here are near-vestigial
(buy returns 501). Store integer cents in the DB (`INTEGER`), compute the wire
value as `cents as f64 / 100.0`. (If we ever want real multi-currency, revisit
doubloon and persist minor units.)

### 1. `AvailabilityState` enum — DONE (code), not committed
- `src/model.rs`: added `enum AvailabilityState { Available, Unavailable,
  Reserved, Ready }` (`#[serde(rename_all = "lowercase")]`); `Availability.state`
  is now that enum instead of `Cow<'static, str>`.
- `src/catalog.rs`: `state: AvailabilityState::Available`.
- Built + `lendable` test green.

### 2. `Format` enum — IN PROGRESS
Goal: one `enum Format { Epub, Xtc, Xtch }` replacing the 4 string helpers and
`BookFile.media_type: String` -> `BookFile.format: Format`. DB `book_files.media_type`
column is UNCHANGED (still stores the media-type string); `Format::from_media_type`
parses it on read, `format.media_type()` writes it. No migration, no `.sqlx` change.
- DONE `src/catalog.rs`: `Format` enum with `from_path`/`from_media_type`/
  `media_type()`/`ext()`/`rank()`/`read_meta()`; `BookFile { path, format }`;
  removed `media_type_for`/`format_rank`/`format_ext`/`read_meta` free fns;
  `is_book_file` now uses `Format::from_path`; `to_publication` loop uses
  `file.format.ext()` / `file.format.media_type()`.
- DONE `src/library.rs`: import `Format`; `files_for` uses `filter_map` +
  `Format::from_media_type` (warn+skip on unknown); `reconcile_dir` and `ingest`
  compute `Format::from_path` and call `format.read_meta`; `ingest_file` now
  takes a `format: Format` arg and uses `format.media_type()` / `format.rank()`.
- TODO `src/main.rs` (the ONLY remaining edits to make Format compile):
  - `download_format` (~line 740): `catalog::format_ext(&f.media_type) == format`
    -> `f.format.ext() == format`.
  - (~line 751): `let media_type = file.media_type.clone();` ->
    `let media_type = file.format.media_type();` (now `&'static str`; drop `.clone()`,
    `file_response` takes `&str`).
  - `serve_cover` (~line 849): `.find(|f| f.media_type == "application/epub+zip")`
    -> `.find(|f| f.format == catalog::Format::Epub)`.
  - test `epub_and_xtc_of_same_work_group_into_one_book` (~line 1491):
    `files.iter().map(|f| f.media_type.clone())` -> `.map(|f| f.format.media_type())`
    collecting `Vec<&str>`; the two `media_types.contains(&"...".to_string())`
    asserts become `.contains(&"application/epub+zip")` etc.
- THEN: `cargo build` + `cargo test` (DATABASE_URL=sqlite:dev.db), commit.

### 3. Money (`UsdCents`) + `Acquisition` enum — NOT STARTED
Replace `Book.price_usd: Option<f64>` + `Book.lendable: bool` (an implicit
tri-state with the impossible `lendable && priced` combo) with:
- `struct UsdCents(u32)` (probably in `catalog.rs` or a small `money.rs`).
- `enum Acquisition { OpenAccess, Buy(UsdCents), Borrow }` on `Book`.
Work:
- `src/catalog.rs`: add the types; `Book` drops `price_usd`/`lendable`, gains
  `acquisition: Acquisition`. `to_publication`'s borrow/buy/open-access `match`
  keys off `self.acquisition` instead of `if lendable / else if price`. Wire
  `Price { value: cents.0 as f64 / 100.0, .. }`.
- New migration `0004_*.sql`: `books.price_usd REAL` -> integer cents. Simplest:
  add `price_cents INTEGER`, backfill `CAST(ROUND(price_usd*100) AS INTEGER)`,
  keep `lendable`. (Acquisition is derived at read time from `price_cents` +
  `lendable`: Some(cents)->Buy, lendable->Borrow, else OpenAccess.) Or add an
  explicit `acquisition` tag column — derived is less churn.
- `src/library.rs`: `BookRow` reads `price_cents`/`lendable` and builds
  `Acquisition`; `create_book`/`write_metadata`/`reset_to_samples` and the many
  column lists (`BOOK_COLUMNS` etc.) updated. Regenerate `.sqlx`
  (`cargo sqlx prepare`, DATABASE_URL=sqlite:dev.db).
- `src/main.rs`: admin/CLI paths that set price/lendable; sample data.
- Tests: `paid_publication_has_indirect_acquisition`,
  `lendable_publication_has_borrow_with_availability` reference price/lendable.

### 4. ID newtypes (`BookId`, `CategorySlug`) — NOT STARTED
Wrap the slug strings for type safety. NOT uuid/int — the ids are human-readable
URL slugs (`/opds/publications/moby-dick`) derived from titles; keep that.
- `struct BookId(String)` / `struct CategorySlug(String)` (Deserialize for axum
  `Path` extractors; `Display`/`AsRef<str>`; sqlx `Type`/encode as text).
- Thread through `Book.id`, `Category.slug`, `CatalogStore` method signatures,
  and the axum handlers. Biggest surface area — do it LAST, one module at a time,
  building between each.
- Regenerate `.sqlx` if any query bindings change types.

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
