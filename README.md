# minerva

An [OPDS 2.0](https://specs.opds.io/opds-2.0.html) and
[OPDS 1.2](https://specs.opds.io/opds-1.2) catalog server built with
[Axum](https://github.com/tokio-rs/axum). One catalog, served in both versions:
OPDS 2.0 under `/opds` and OPDS 1.2 under `/opds1`.

OPDS 2.0 is built on the Readium Web Publication Manifest model: everything is a
JSON *collection* made of `metadata`, `links`, and sub-collections
(`navigation`, `publications`, `facets`, `groups`). Feeds are served as
`application/opds+json` and individual publications as
`application/opds-publication+json`.

OPDS 1.2 predates that model: a catalog is an Atom feed, either a *navigation*
feed whose entries link to further feeds or an *acquisition* feed whose entries
are publications, and the OPDS vocabulary (prices, indirect acquisition,
lending availability) rides along in the `opds:` XML namespace. Most existing
readers — KOReader, Moon+ Reader, Aldiko, Thorium — speak this version.

### Which version should a client use?

Point the client at `/opds` if it speaks OPDS 2.0, and at `/opds1` otherwise.
Each root carries an `alternate` link to the other, so either one is a usable
starting point.

The two versions live at separate URLs rather than negotiating on `Accept`.
OPDS 1.x clients commonly send `Accept: */*`, and every href inside an Atom
feed has to resolve to Atom for browsing to continue — so a feed's version is
settled by its path, not by a header the client may not send.

## Running

```sh
# a library directory of EPUBs is required
OPDS_LIBRARY_DIR=/path/to/epubs cargo run
# the catalog + accounts are stored in SQLite (default ./opds.db)
OPDS_LIBRARY_DIR=/path/to/epubs OPDS_DB=/var/lib/opds/catalog.db cargo run
# override the base URL used to build absolute hrefs (default http://localhost:3000)
OPDS_LIBRARY_DIR=/path/to/epubs OPDS_BASE_URL=https://books.example.com cargo run
# add an account (stored in the same OPDS_DB); the catalog then requires login
cargo run adduser alice        # prompts for a password (hidden, confirmed)
```

Each of these environment variables is also a command-line flag (`--base-url`,
`--db`, `--library-dir`); a flag takes precedence over its variable. Run
`cargo run -- --help` for the full CLI.

The server listens on `0.0.0.0:3000`. Visit http://localhost:3000/opds.

The catalog and the user accounts live in one SQLite database (`OPDS_DB`, in a
`books` and a `users` table). HTTP Basic auth is enforced whenever at least one
account exists, and the catalog is open otherwise.

## Catalog source

The catalog lives in a SQLite `books` table (`OPDS_DB`) and is queried per
request rather than held in memory.

`OPDS_LIBRARY_DIR` is required. On startup the server reconciles the store
against it — scanning book files (`*.epub`, `*.xtc`, `*.xtch`) recursively and
recording each file's metadata (title, author, language, description, subjects,
series) and cover. A book is a logical work that can have **several format files**:
files sharing a title + author group into one publication with one download link
per format, and the richest format (EPUB over XTC) supplies the shared metadata.
Unchanged files (matching a stored modification time) are skipped on restart, so
startup is cheap for large, mostly-static libraries. The directory is
**watched**: adding a book file inserts/attaches a format and removing one
detaches it (deleting the book once its last format is gone) — no restart
required. Downloads stream the real file bytes and cover requests serve the
image embedded in the EPUB (XTC covers use a bespoke page codec and are not
extractable, so those fall back to a generated SVG).

(The built-in sample catalog and its generated EPUBs are test-only scaffolding
and are not compiled into the server.)

## Endpoints

### Shared

| Method & path                  | Description                                             |
| ------------------------------ | ------------------------------------------------------- |
| `GET /`                        | Redirects to `/opds`.                                   |
| `GET /opds/download/{id}/{format}` | Open-access download of one format (`epub`/`xtc`/`xtch`), streamed from disk. |
| `GET /opds/download/{id}.epub` | Open-access download of a sample book: a generated minimal EPUB 3. |
| `GET /opds/covers/{id}`        | A book's cover (embedded image, or a generated SVG).    |
| `GET /opds/covers/{id}/thumb`  | The thumbnail form of the same.                         |
| `GET /opds/buy/{id}`           | Advertised for spec completeness; returns 501 (no store).|
| `GET /opds/borrow/{id}`        | Advertised for lendable titles; returns 501 (no lending).|
| `GET /opds/auth`               | Authentication document (when auth is enabled).         |

Both catalogs link to these: they serve bytes rather than feeds, so there is
nothing version-specific about them.

### OPDS 2.0

| Method & path                  | Description                                             |
| ------------------------------ | ------------------------------------------------------- |
| `GET /opds`                    | Root feed: navigation, a "New Publications" **group**, and a browse group. |
| `GET /opds/all?page=N`         | Paginated **acquisition** feed of all publications, with facets and pagination links. |
| `GET /opds/category/{slug}`    | Acquisition feed for a category.                        |
| `GET /opds/authors/{slug}`     | Acquisition feed for an author.                         |
| `GET /opds/publications/{id}`  | A single publication document.                          |
| `GET /opds/publications/{id}/categories` | JSON list of a publication's categories.      |
| `POST /opds/publications/{id}/categories` | Assign a category: `{"name": "Sci-Fi"}` (created on demand). |
| `DELETE /opds/publications/{id}/categories/{slug}` | Remove a category from a publication. |
| `GET /opds/search?query=...`   | Search feed; also accepts `author=` and `title=` field filters. |

### OPDS 1.2

| Method & path                     | Feed kind                                            |
| --------------------------------- | ---------------------------------------------------- |
| `GET /opds1`                      | **Navigation**: the catalog root, one entry per browsable view. |
| `GET /opds1/all?page=N`           | **Acquisition**: every publication, paginated, with category facets. |
| `GET /opds1/new`                  | **Acquisition**: the most recently added titles.     |
| `GET /opds1/categories`           | **Navigation**: one entry per category, with counts. |
| `GET /opds1/category/{slug}`      | **Acquisition**: one category.                       |
| `GET /opds1/authors`              | **Navigation**: one entry per author, with counts.   |
| `GET /opds1/authors/{slug}`       | **Acquisition**: one author.                         |
| `GET /opds1/publications/{id}`    | A single "complete entry" document.                  |
| `GET /opds1/search?query=...`     | **Acquisition**: search results (same filters as 2.0). |
| `GET /opds1/opensearch.xml`       | The OpenSearch description document.                 |

OPDS 1.x has no equivalent of a 2.0 `group`, so the category and author browse
groups that the 2.0 root feed inlines become navigation feeds of their own at
`/opds1/categories` and `/opds1/authors`.

## What's implemented

- The core collection model: feeds with `metadata`, `links`, `navigation`,
  `publications`, `facets`, and `groups` (the root feed groups a publications
  preview and a category-browse navigation collection, each with its own
  metadata and `self` link).
- Link objects with `rel`, `type`, `title`, `templated`, and `properties`.
- Arbitrary, many-to-many categories (a `categories`/`book_categories` table
  pair), assignable/removable at runtime via the publication category
  endpoints. A newly-scanned book is filed under **every** category derived
  from it: its top-level library subfolder (whatever it is called — `Cook
  Books`, `Programming`, `D&D 5e`) plus any genre recognized in its own
  `dc:subject` values. See [Deriving categories](#deriving-categories). The facet, browse group, and
  `/opds/category/{slug}` feed are all driven from the table.
- A filesystem-backed catalog (`OPDS_LIBRARY_DIR`) that scans EPUB and XTC/XTCH
  files for metadata and covers and live-reloads on additions/removals, grouping
  multiple formats of the same work into one publication.
- Acquisition links: one free `open-access` download per available format, paid
  `buy` links (with a `price` and an `indirectAcquisition`), and library
  `borrow` links carrying lending `availability`/`copies`/`holds` (an OPDS
  extension). Downloads stream the real file bytes (or a generated minimal
  EPUB 3 for samples); buy and borrow are advertised but report 501.
- Cover `images` (full-size + thumbnail), served from the EPUB's embedded cover
  (thumbnails are downscaled to fit 160x240 and re-encoded as JPEG) or as a
  generated SVG placeholder.
- Series metadata (`belongsTo.series` with `name`/`position`), read from EPUB
  Calibre or EPUB3 collection metadata and editable in the admin UI.
- A templated `search` link (`search{?query,author,title}`) and a search
  endpoint supporting a general query plus per-field author/title filters.
- Pagination on the acquisition feed: `numberOfItems`/`itemsPerPage`/`currentPage`
  metadata plus `first`/`previous`/`next`/`last` links.
- Optional multi-account HTTP Basic authentication (accounts in the `users`
  table of `OPDS_DB`), with Argon2-hashed passwords and constant-time
  verification (RustCrypto). Manage accounts with the `adduser` subcommand;
  auth is enforced whenever an account exists. Protected resources answer 401
  with an Authentication for OPDS document (`application/opds-authentication+json`),
  also served (unprotected) at `/opds/auth`.
- Correct OPDS media types on every response.

### OPDS 1.2 specifically

- Navigation and acquisition feeds, distinguished by the `kind` parameter of
  their `application/atom+xml;profile=opds-catalog` media type, plus
  "complete entry" documents for single publications.
- The full Atom required set on every feed and entry (`id`, `title`,
  `updated`), with namespaced extensions for everything else: `dcterms:language`,
  `schema:Series` (name + position), `opds:price` / `opds:indirectAcquisition`,
  `opds:availability` / `opds:copies` / `opds:holds`, and `thr:count`.
- Acquisition links mirroring the 2.0 catalog: one `open-access` link per
  available format (EPUB first — clients pick a format by media type and ignore
  the ones they don't know, XTC/XTCH among them), or a `buy`/`borrow` link
  carrying the price or the lending availability.
- Pagination as RFC 5005 `first`/`previous`/`next`/`last` links plus OpenSearch
  `totalResults`/`itemsPerPage`/`startIndex` counts.
- Facets as feed-level links grouped by `opds:facetGroup`, counted with
  `thr:count` — the 1.x spelling of the 2.0 `facets` collection.
- Search through an OpenSearch description document (`rel="search"`), rather
  than 2.0's templated link.
- All text escaped on write, so metadata containing `&` or `<` (common in
  real EPUBs) cannot produce a malformed feed.

## Tests

```sh
cargo test
```

Integration tests drive the fully-wired router (via `tower::ServiceExt::oneshot`)
and cover the root feed, pagination, category filtering, publication documents,
search, EPUB/cover/buy asset endpoints, and 404s — for both catalog versions.
The OPDS 1.x tests parse each response with `roxmltree` and assert on the
parsed tree (feed kinds, link rels, facet attributes, OpenSearch counts), and
the `atom` module's own unit tests cover the wire format directly, including
that nasty metadata round-trips through an XML parser unchanged. A directory-scan test writes a
generated EPUB to a temp dir and confirms it is picked up and then dropped after
removal. Tests run against an in-memory SQLite database.

## Development

Data access uses [sqlx](https://github.com/launchbadge/sqlx) with compile-time
checked queries. A checked-in offline cache (`.sqlx/`) lets the project build
without a database, so `cargo build` and `cargo test` work out of the box.

If you change any SQL (or the schema in `migrations/`), regenerate the cache:

```sh
export DATABASE_URL=sqlite:dev.db
sqlx database create && sqlx migrate run   # one-time: create the dev database
cargo sqlx prepare                         # refresh .sqlx/ — commit the result
```

## Deriving categories

A library's folder layout is usually its owner's taxonomy, so the top-level
subfolder under `OPDS_LIBRARY_DIR` becomes a category as-is. A folder named
after the book's own author is skipped, since the author browse feed already
covers that. `Fiction` and `Non-Fiction` keep their canonical slugs however
they are spelled, so one browse entry never splits into two
identically-labelled halves.

On top of that, each `dc:subject` in the book's own metadata is normalized by
`src/subjects.rs` onto a small fixed set of genres (`Fantasy`,
`Science Fiction`, `Mystery`, `Cooking`, `Programming`, ...). Subjects arrive
as bare genres, BISAC paths (`COMPUTERS / Programming Languages / Python`), or
comma-joined lists of them, so values are split on `,`, `/` and `;` and every
resulting term is looked up. Matching is on whole terms, never substrings —
`Science Fiction` must not be filed under `Science`.

**Anything unrecognized is dropped rather than guessed.** Real subject lists
are full of things that are true but useless to browse (`New York Times
bestseller`, `Large type books`, `Rincewind the wizard (fictitious
character)`), and filing books under wrong categories is worse than leaving
them in the broad bucket their folder already gives them. Every book still
lands in at least one category: with no usable folder and no recognized
subject, a broad fiction/non-fiction guess stands in.

To widen what is recognized, add entries to `SUBJECT_MAP` in
`src/subjects.rs`.

### Applying rule changes to an existing library

Scanning skips files whose mtime is unchanged, and only seeds categories for
books it creates, so changing the rules does not by itself reach a library
that is already ingested. `recategorize` re-reads each book's file and applies
them:

```sh
# add newly-derived categories; nothing is removed
cargo run -- recategorize
# also clear the blanket Fiction/Non-Fiction guess where a better one was found
cargo run -- recategorize --prune
```

Hand-assigned categories are never touched. `--prune` only removes a blanket
guess that the current rules no longer derive for that book, so a book that
really does live in `Fiction/` keeps it.

## Management subcommands

Besides `adduser`, the binary offers subcommands for editing the catalog
directly (they operate on `OPDS_DB` and exit):

```sh
cargo run -- set-title <id> "New Title"
cargo run -- set-author <id> "New Author"
cargo run -- add-category <id> "Science Fiction"   # created on demand
cargo run -- remove-category <id> <category-slug>
cargo run -- remove-book <id>
cargo run -- recategorize [--prune]   # re-derive categories (needs the library dir)
```

Note: for file-backed books, edits to title/author persist until the EPUB file
changes and is re-scanned.

## Web admin

A small management UI is served at `/admin` (behind auth when it is enabled).
It lists every book with inline forms to edit the title/author, add/remove
categories, and remove the book, plus an EPUB upload form. Uploads are saved
into `OPDS_LIBRARY_DIR` (required for uploads) and reconciled immediately.

## Deployment

A hardened systemd unit is provided at [`minerva.service`](minerva.service);
its header comments cover installing the binary, creating the service user, and
configuring it (env vars or `/etc/minerva/minerva.env`). The catalog
database lives in `/var/lib/minerva`.

## Layout

- `src/model.rs` — serde types for the OPDS 2.0 wire format, plus the link
  relations and acquisition vocabulary shared with OPDS 1.x.
- `src/atom.rs` — the OPDS 1.2 wire format: Atom feeds, entries and the
  OpenSearch description document.
- `src/opds1.rs` — the OPDS 1.2 handlers and routes (`/opds1`).
- `src/catalog.rs` — the `Book`/`Category` domain types, the sample set, and
  library scanning helpers (per-format media types, work grouping).
- `src/library.rs` — the SQLite-backed catalog store (queries + reconciliation),
  including the `book_files` format table.
- `src/db.rs` — the sqlx connection pool + migrations.
- `src/xtc.rs` — reads metadata out of XTC/XTCH files.
- `migrations/` — SQL schema migrations (applied at startup).
- `src/epub.rs` — reads metadata and cover images out of EPUB files.
- `src/subjects.rs` — normalizes EPUB `dc:subject` values onto a small
  browsable set of genres.
- `src/covers.rs` — placeholder SVG covers and JPEG thumbnail generation.
- `src/assets.rs` — EPUB generation (test/demo scaffolding; compiled for tests only).
- `src/watch.rs` — watches the library directory and updates the catalog store.
- `src/auth.rs` — the SQLite-backed user store and Argon2 password hashing.
- `src/admin.rs` — the tera-templated web management UI.
- `src/main.rs` — the Axum router, handlers, auth middleware, and response wrapper.
