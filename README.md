# opds-axum

An [OPDS 2.0](https://specs.opds.io/opds-2.0.html) catalog server built with
[Axum](https://github.com/tokio-rs/axum).

OPDS 2.0 is built on the Readium Web Publication Manifest model: everything is a
JSON *collection* made of `metadata`, `links`, and sub-collections
(`navigation`, `publications`, `facets`, `groups`). Feeds are served as
`application/opds+json` and individual publications as
`application/opds-publication+json`.

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
recording each file's metadata (title, author, language, description, subjects)
and cover. A book is a logical work that can have **several format files**:
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

| Method & path                  | Description                                             |
| ------------------------------ | ------------------------------------------------------- |
| `GET /`                        | Redirects to `/opds`.                                   |
| `GET /opds`                    | Root feed: navigation, a "New Publications" **group**, and a browse group. |
| `GET /opds/all?page=N`         | Paginated **acquisition** feed of all publications, with facets and pagination links. |
| `GET /opds/category/{slug}`    | Acquisition feed for a category.                        |
| `GET /opds/publications/{id}`  | A single publication document.                          |
| `GET /opds/publications/{id}/categories` | JSON list of a publication's categories.      |
| `POST /opds/publications/{id}/categories` | Assign a category: `{"name": "Sci-Fi"}` (created on demand). |
| `DELETE /opds/publications/{id}/categories/{slug}` | Remove a category from a publication. |
| `GET /opds/search?query=...`   | Search feed; also accepts `author=` and `title=` field filters. |
| `GET /opds/download/{id}/{format}` | Open-access download of one format (`epub`/`xtc`/`xtch`), streamed from disk. |
| `GET /opds/download/{id}.epub` | Open-access download of a sample book: a generated minimal EPUB 3. |
| `GET /opds/buy/{id}`           | Advertised for spec completeness; returns 501 (no store).|
| `GET /opds/borrow/{id}`        | Advertised for lendable titles; returns 501 (no lending).|
| `GET /opds/covers/{id}.svg`    | Generated SVG cover (`{id}-thumb.svg` for the thumbnail).|
| `GET /opds/auth`               | Authentication document (when auth is enabled).         |

## What's implemented

- The core collection model: feeds with `metadata`, `links`, `navigation`,
  `publications`, `facets`, and `groups` (the root feed groups a publications
  preview and a category-browse navigation collection, each with its own
  metadata and `self` link).
- Link objects with `rel`, `type`, `title`, `templated`, and `properties`.
- Arbitrary, many-to-many categories (a `categories`/`book_categories` table
  pair): scanning files them under a default category (library subfolder or
  subject heuristic), and they can be assigned/removed at runtime via the
  publication category endpoints. The facet, browse group, and
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

## Tests

```sh
cargo test
```

Integration tests drive the fully-wired router (via `tower::ServiceExt::oneshot`)
and cover the root feed, pagination, category filtering, publication documents,
search, EPUB/cover/buy asset endpoints, and 404s. A directory-scan test writes a
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

## Management subcommands

Besides `adduser`, the binary offers subcommands for editing the catalog
directly (they operate on `OPDS_DB` and exit):

```sh
cargo run -- set-title <id> "New Title"
cargo run -- set-author <id> "New Author"
cargo run -- add-category <id> "Science Fiction"   # created on demand
cargo run -- remove-category <id> <category-slug>
cargo run -- remove-book <id>
```

Note: for file-backed books, edits to title/author persist until the EPUB file
changes and is re-scanned.

## Web admin

A small management UI is served at `/admin` (behind auth when it is enabled).
It lists every book with inline forms to edit the title/author, add/remove
categories, and remove the book, plus an EPUB upload form. Uploads are saved
into `OPDS_LIBRARY_DIR` (required for uploads) and reconciled immediately.

## Deployment

A hardened systemd unit is provided at [`opds-axum.service`](opds-axum.service);
its header comments cover installing the binary, creating the service user, and
configuring it (env vars or `/etc/opds-axum/opds-axum.env`). The catalog
database lives in `/var/lib/opds-axum`.

## Layout

- `src/model.rs` — serde types for the OPDS 2.0 wire format.
- `src/catalog.rs` — the `Book`/`Category` domain types, the sample set, and
  library scanning helpers (per-format media types, work grouping).
- `src/library.rs` — the SQLite-backed catalog store (queries + reconciliation),
  including the `book_files` format table.
- `src/db.rs` — the sqlx connection pool + migrations.
- `src/xtc.rs` — reads metadata out of XTC/XTCH files.
- `migrations/` — SQL schema migrations (applied at startup).
- `src/epub.rs` — reads metadata and cover images out of EPUB files.
- `src/covers.rs` — placeholder SVG covers and JPEG thumbnail generation.
- `src/assets.rs` — EPUB generation (test/demo scaffolding; compiled for tests only).
- `src/watch.rs` — watches the library directory and updates the catalog store.
- `src/auth.rs` — the SQLite-backed user store and Argon2 password hashing.
- `src/admin.rs` — the tera-templated web management UI.
- `src/main.rs` — the Axum router, handlers, auth middleware, and response wrapper.
