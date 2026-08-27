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
cargo run
# override the base URL used to build absolute hrefs (default http://localhost:3000)
OPDS_BASE_URL=https://books.example.com cargo run
```

The server listens on `0.0.0.0:3000`. Visit http://localhost:3000/opds.

## Endpoints

| Method & path                  | Description                                             |
| ------------------------------ | ------------------------------------------------------- |
| `GET /`                        | Redirects to `/opds`.                                   |
| `GET /opds`                    | Root **navigation** feed (entry point + search link).   |
| `GET /opds/all?page=N`         | Paginated **acquisition** feed of all publications, with facets and pagination links. |
| `GET /opds/category/{slug}`    | Acquisition feed for a category (`fiction`/`nonfiction`).|
| `GET /opds/publications/{id}`  | A single publication document.                          |
| `GET /opds/search?query=...`   | Search feed (matches title, author, description).       |
| `GET /opds/download/{id}.epub` | Open-access download: a generated minimal EPUB 3.       |
| `GET /opds/buy/{id}`           | Placeholder purchase page for a paid title.             |
| `GET /opds/covers/{id}.svg`    | Generated SVG cover (`{id}-thumb.svg` for the thumbnail).|

## What's implemented

- The core collection model: feeds with `metadata`, `links`, `navigation`,
  `publications`, `facets`.
- Link objects with `rel`, `type`, `title`, `templated`, and `properties`.
- Acquisition links: free `open-access` downloads and paid `buy` links carrying
  a `price` (currency + value) — both backed by working endpoints. Open-access
  links serve a generated, structurally-valid minimal EPUB 3.
- Cover `images` (full-size + thumbnail) with dimensions, served as generated
  SVG placeholders.
- A templated `search` link and a working search endpoint.
- Pagination on the acquisition feed: `numberOfItems`/`itemsPerPage`/`currentPage`
  metadata plus `first`/`previous`/`next`/`last` links.
- Correct OPDS media types on every response.

## Tests

```sh
cargo test
```

Integration tests drive the fully-wired router (via `tower::ServiceExt::oneshot`)
and cover the root feed, pagination, category filtering, publication documents,
search, EPUB/cover/buy asset endpoints, and 404s.

## Layout

- `src/model.rs` — serde types for the OPDS 2.0 wire format.
- `src/catalog.rs` — in-memory sample catalog and its mapping to publications.
- `src/assets.rs` — on-the-fly EPUB and SVG cover generation.
- `src/main.rs` — the Axum router, handlers, and response wrapper.

The catalog is a fixed in-memory list of public-domain titles. Swapping it for a
database or filesystem scan only requires changing `src/catalog.rs`.
