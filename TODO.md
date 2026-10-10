# Category derivation (Tier 0 — no external API) — DONE

Browsing was useless: 192 books, 97% of them in `Fiction` (141) or
`Non-Fiction` (51). Two causes, both fixed without touching the network.

**1. The library's own folder layout was being discarded.** `derive_category`
only recognized a top-level `Fiction`/`Non-Fiction` folder, so `Cook Books`,
`Programming`, `D&D 5e` and `french` (41 books) fell through to the subject
heuristic and all landed in Non-Fiction. The top-level subfolder is now a
category whatever it is named.

**2. Books' own `dc:subject` values were only used as a fiction/non-fiction
hint.** 32% of the library has subjects, and the Pratchett bulk self-labels as
`Fantasy` (48) and `Discworld` (46). `src/subjects.rs` now normalizes them
onto a small fixed genre set.

Measured on the real catalog (against copies; `opds.db` untouched):

    before:  5 categories — Fiction 141, Non-Fiction 51, fantasy 2, WoT 2, discworld 1
    after : 17 categories — Fiction 141, fantasy 55, D&D 5e 15, Non-Fiction 14,
                            Programming 12, Cook Books 10, Adventure 5, French 3,
                            Humor 2, WoT 2, Children's 1, discworld 1, Horror 1,
                            Romance 1, Science Fiction 1, Thriller 1, Young Adult 1

`recategorize` applied 109 additions and pruned 40 stale blanket guesses.
Non-Fiction went 51 -> 14 as books moved to their real homes. No book is
uncategorized.

Decisions worth keeping:

- **Unrecognized subjects are dropped, never guessed.** Real subject lists are
  full of true-but-unbrowsable noise (`New York Times bestseller`, `Large type
  books`, `Rincewind the wizard (fictitious character)`). Same reasoning as
  the earlier decision not to guess an author from the folder path: a silent
  wrong category is worse than a broad right one. Widening the taxonomy means
  adding to `SUBJECT_MAP`.
- **Whole-term matching, not substrings** — otherwise `Science Fiction` files
  under `Science`. Every multi-word variant is spelled out in the map.
- **`Fiction`/`Non-Fiction` keep their historical slugs.** Caught by an
  existing test: slugifying the folder name mints `non-fiction` beside the
  stored `nonfiction`, splitting one browse entry into two identically-
  labelled halves. `canonical_category` pins both spellings, and
  `assign_category` now routes through it too — so typing "Non-Fiction" in the
  admin page no longer creates a duplicate either. That second one was a live
  bug on main, not something this change introduced.
- **A folder named after the book's own author is skipped**, or a library laid
  out as `Books/<Author>/*.epub` would mint a category per author and duplicate
  the author browse feed.
- **Directory matching is by filesystem identity, not spelling.** macOS
  firmlinks give one directory two equally-real absolute paths (`/Users/...`
  and `/System/Volumes/Data/Users/...`) and `canonicalize` reconciles neither
  — it returns each unchanged. Book paths come from the DB while the library
  directory comes from a flag, so the two spellings meet routinely. The first
  implementation used `canonicalize` and silently derived zero folder
  categories against the real library; `relative_to` now falls back to
  comparing `(dev, ino)` while walking up from the file.
- **`recategorize` adds; `--prune` is opt-in and narrow.** Hand-assigned
  categories are never touched, and a blanket guess is removed only when the
  current rules no longer derive it for that book — so a book genuinely in
  `Fiction/` keeps `Fiction`.
- Built entirely from existing queries, so the `.sqlx` cache needed no
  regeneration and there is no migration.

Known cosmetic wart: the pre-existing hand-made `fantasy` category keeps its
lowercase label (`seed_category` is `INSERT OR IGNORE`, so the stored label
wins) and now shows as `fantasy` next to `Fiction` and `D&D 5e`. Deliberately
not clobbered — it is a label the owner typed. A one-line change in
`recategorize` could upgrade case-only label differences if wanted.

## Next lever for the 141-book Fiction pile (not done)

`Discworld` appears as a subject on 46 books and is a *series*, not a
category — and series metadata is already parsed, stored and on the wire
(`belongsTo.series`) with no browse feed over it. A "Browse by Series" group
plus `/opds/series/{slug}` would split the biggest bucket using data already
in the database, again with no external API. Probably worth more than Tier 1.

## Tier 1 (external API) — researched, not built

Measured on 15 real books, matching by ISBN:

| Service | Key? | Hit rate | Returns |
| --- | --- | --- | --- |
| Open Library | no | 12/15 | subjects, long-tail and noisy |
| LC SRU (`lx2.loc.gov:210/LCDB`) | no | 4/15 (3 with a usable LCC class) | LCSH + LCC, controlled, high quality |
| Google Books | yes, in practice | untested | BISAC categories, few and clean |
| Goodreads | unobtainable | n/a | n/a |

- **Goodreads is dead**: the endpoint answers only `Invalid API key`, and new
  keys stopped being issued in Dec 2020.
- **`loc.gov/apis` is the right index**, but its main JSON API "does not
  include records from the library catalog" — digitized items only, so it
  cannot resolve a commercial EPUB by ISBN. The catalog lives behind SRU, and
  LCSH/LCC behind the Linked Data Service (`id.loc.gov`).
- LC's low hit rate is structural: LC catalogs *print* editions and these are
  ebook ISBNs. Also `050` is not always a class — Hogfather's came back as
  `CPB Box no. 1955 vol. 14`, a shelf location, so it needs validating
  against `^[A-Z]{1,3}[0-9]`.
- **Google Books keyless is unusable**: the anonymous quota is shared and
  already exhausted (HTTP 429, `project_number:624717413613`). A key gets its
  own 1k/day.
- Prerequisite either way: **persist the ISBN**. `EpubMeta.identifier` is
  already parsed from `dc:identifier` and then thrown away — 71% of the
  library (137/191) has a real ISBN in it, and it is the only reliable join
  key. Not in `Book`, not in the schema.
- Prerequisite for any title+author fallback: author cleanup. `Terry
  Pratchett` (55) and `Pratchett, Terry` (9) are one person, and `HTML to
  Epub` (14) is a conversion artifact that will match nothing.
- Shape if built: a "Suggest categories" button per book in the admin page →
  Open Library for coverage, LC for an LCC class when present → normalize
  through `subjects.rs` → render as clickable chips the owner confirms. Never
  auto-apply: the whole point of the normalization decision above is that
  wrong categories are worse than broad ones.

# OPDS 1.x support — DONE

OPDS 1.2 is served under `/opds1`, alongside the existing OPDS 2.0 catalog at
`/opds`. Same store, same domain types, same asset endpoints; two presentations.

Decisions worth keeping:

- **Separate URL prefix, not `Accept` negotiation.** 1.x clients commonly send
  `Accept: */*`, and every href inside an Atom feed must resolve to Atom for
  browsing to continue — negotiation would depend on a header the client may
  not send, on every subsequent request. Confirmed during the smoke test: curl
  (and the clients that behave like it) sends exactly `accept: */*`. Each root
  carries an `alternate` link to the other version instead.
- **`quick-xml` 0.42 (new dep), imperative `Writer`.** `roxmltree` (already in
  tree) only parses. Hand-rolled string building risks an escaping bug on
  untrusted EPUB metadata; `quick-xml`'s `BytesText::new` and attribute values
  escape on write. Serde XML fights namespaces; tera templates lose type
  safety on conditional link sets. No transitive deps added.
- **Duplicate the handlers, share the policy.** A "shared view layer, two
  renderers" refactor of the nine 2.0 handlers wasn't worth it. What *is*
  shared, because it would otherwise drift: `page_window` (pagination
  arithmetic), `SearchParams::{terms, query_string}` (term normalization and
  the self-link echo), `Book::acquisition_properties` (the lending/price
  policy, including the hardcoded demo copy counts), `Book::download_paths`
  (the per-format download URL scheme), `Book::cover_media_type`, and
  `model::rel` (the OPDS link relation URIs).
- **No `groups` in 1.x**, so the 2.0 root's category and author browse groups
  became navigation feeds of their own: `/opds1/categories`, `/opds1/authors`.
  A category feed's `up` link leads to its index, not to the flat feed.
- **`<updated>` is required by Atom** but `Book::modified` is optional, so an
  entry with no recorded file mtime falls back to feed-build time.
- **EPUB first in the acquisition links.** Clients dispatch on the link `type`
  and ignore media types they don't know — `application/x-xtc` means nothing
  to KOReader or Thorium, so format order decides whether an entry is usable.
- **`<summary type="text">` on feed entries, `<content type="html">` only in
  the complete-entry document.** Clients read `summary` more reliably; the
  description may carry markup, and `html` is its correct Atom carrier.

New files: `src/atom.rs` (wire format), `src/opds1.rs` (handlers + routes).
Tests: 11 `atom` unit tests (including that `&`/`<`-bearing metadata
round-trips through an XML parser unchanged) + 15 integration tests driving the
wired router and asserting on the `roxmltree`-parsed tree. 64 tests pass.

Verified end to end against a real library directory: all 20 URLs reachable
from `/opds1` return 200 with the right media type, every feed passes
`xmllint`, the live watcher's ingest shows up in the Atom feeds, and escaped
EPUB metadata round-trips exactly.

Not done, deliberately:

- `Accept`-based redirect from `/` or `/opds` to `/opds1`. Listed as optional
  polish in the plan; it risks surprising a 2.0 client, and the `alternate`
  cross-links already make each version discoverable.
- Per-entry `<category>` elements in *feeds*. The complete-entry document has
  them (one store call); doing it for a page of entries wants a batched
  lookup rather than N+1.

## Pre-existing CLI bug found while verifying — FIXED

`-l` was claimed by both `--listen` (`short = 'l'`) and `--library-dir` (bare
`short`, derived to `-l`). Clap's duplicate-short check is a `debug_assert`, so
**every debug build panicked before `main`**:

    Command minerva: Short option names must be unique for each argument,
    but '-l' is in use by both 'listen' and 'library_dir'

Release builds have the assert compiled out and started fine, which is why it
went unnoticed since `d59c7a4` — but `cargo run` could not start the server at
all.

Fixed by dropping the short option from *both* arguments rather than picking a
winner: `--listen` and `--library-dir` are now long-form only, so there is no
ambiguity to resolve and no muscle memory to mislead. `-u`/`--base-url` and
`-d`/`--db` keep their shorts. Verified: `./target/debug/minerva --help` builds
its command, and the debug server now boots and serves the full catalog.

- [x] admin page still has old name — now "minerva"
- [x] admin page reloads w/o scroll position on category add — mutations are now
      fetch()-based AJAX; the page updates in place, no reload
- [x] categories should be case insensitive — already were (slugify lowercases,
      categories.slug is the PK so "Fiction"/"fiction" collapse); add_category now
      returns the canonical category as JSON so the chip's slug matches the server
- [x] admin page should link to book downloads — new Download column
- [x] lots of my books are missing authors — root cause (from a sample) was
      Calibre exporting a literal `<dc:creator>Unknown</dc:creator>`; the real
      author is only in the folder layout. Deliberately did NOT guess the author
      from the folder path (fragile — genre/flat layouts would produce silent
      wrong authors). Instead: keep the `opf:file-as` parsing fallback, normalize
      placeholder authors ("", "Unknown", "Unknown Author") to one canonical
      value, and add an admin filter (`/admin?unknown=1`) listing books that need
      an author so they can be fixed inline.
- [x] book slugs replace accented letters with `-` — slugify now transliterates
      via deunicode ("République" -> "republique")
- [x] A way to note what number in a series of a book would be good — series +
      series_index parsed from EPUB (Calibre + EPUB3), stored, on the wire
      (belongsTo.series), and editable in admin

Only ID newtypes (refactor #4 below) remains.

# Plan / deferred work

## DONE — "stringly-typed -> richer types" refactor

All four approved pieces are committed (AvailabilityState enum, Format enum,
UsdCents + Acquisition enum, BookId/CategorySlug newtypes). Details below.

Still open: "lots of my books are missing authors" ([~] above) — added the
`file-as` fallback, but a sample EPUB from the real library is needed to confirm
the actual cause (or whether it's XTC files with no embedded author).

Money decision: use a `UsdCents(u32)` newtype, NOT the `doubloon` crate.
doubloon is built on `rust_decimal` (no clean SQLite mapping — SQLite has no
decimal type) and its serde emits the amount as a *string*, whereas the OPDS
wire wants `price.value` as a JSON *number*. Prices here are near-vestigial
(buy returns 501). Store integer cents in the DB (`INTEGER`), compute the wire
value as `cents as f64 / 100.0`. (If we ever want real multi-currency, revisit
doubloon and persist minor units.)

### 1. `AvailabilityState` enum — DONE + committed
- `src/model.rs`: added `enum AvailabilityState { Available, Unavailable,
  Reserved, Ready }` (`#[serde(rename_all = "lowercase")]`); `Availability.state`
  is now that enum instead of `Cow<'static, str>`.
- `src/catalog.rs`: `state: AvailabilityState::Available`.
- Built + `lendable` test green.

### 2. `Format` enum — DONE + committed
(All the sub-steps below are complete; 33 tests pass, offline build clean.)
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

### 3. Money (`UsdCents`) + `Acquisition` enum — DONE + committed
(`UsdCents(u32)` + `Acquisition { OpenAccess, Buy(UsdCents), Borrow }` on `Book`;
migration `0004_price_cents.sql` drops `price_usd REAL` for `price_cents INTEGER`;
`.sqlx` regenerated; 33 tests pass. Original plan notes kept below for reference.)
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

### 4. ID newtypes (`BookId`, `CategorySlug`) — DONE + committed
(Transparent String newtypes on `Book.id`/`Category.slug`; persistence stays
`&str` via `as_str()`, so no sqlx `Type`/`Encode` impls were needed. 36 tests
pass.) Original plan notes below.
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
