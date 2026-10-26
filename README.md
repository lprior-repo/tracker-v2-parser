# Track scrapper v2

A Rust CLI for collecting published athletics data into PostgreSQL and exporting
observed, comparable results to XLSX. The reference `athletic-rust-pipeline` uses
Fjall and Restate; this application uses neither for its runtime storage or
orchestration.

## Prerequisites

- PostgreSQL and a database URL with permission to apply the SQLx migrations.
- The Rust toolchain pinned in `rust-toolchain.toml`.
- Moon 2.2.4 for the project tasks.
- For database integration tests, `TEST_DATABASE_URL` pointing to a disposable
  PostgreSQL database where the test user may create and drop schemas.
- Browser transport is currently disabled: the WebDriver adapter cannot yet
  enforce public-address restrictions and per-request pacing for Chromium.

## Configuration and commands

Edit `config.toml` to select states, graduation years, source transports, request
limits, and export settings. Set `DATABASE_URL` to a PostgreSQL URL, or supply
`--database-url` on the command line. The CLI also loads a local `.env` file if
one exists; keep credentials out of version control.

```sh
cargo run -- scrape --config config.toml
cargo run -- export --config config.toml --output athletes.xlsx
cargo run -- scrape --help
cargo run -- export --help
```

Scraping uses bounded attempts and checkpoints; access challenges require human
handling. Export reads database records without scraping, writes new XLSX files,
and does not overwrite an existing output. The Issues worksheet identifies
unresolved or incomplete data. See [the scraper contract](docs/contract.md) for
identity, comparison, and checkpoint requirements.

Athletic.net results without a published result ID use its row code where
available (including the meet API's `ShortCode`). Without a code, the key
includes the original source-array coordinates and a digest of the entire
published row, not the mark alone. Reordering or changing such a row can create
duplicates, but cannot overwrite a different row merely because it occupied the
same coordinates. Existing keys are not rewritten or deleted; refreshing older
records can also create duplicates requiring manual reconciliation. No automatic
match is safe when the provider supplies insufficient identity evidence.
Other rows without either an ID or source row use their page position and full
parsed result content, with the same duplication risk after changes or reorder.
MileSplit raw-result identifiers likewise include the full published line as
well as its coordinates; correcting or moving a line can therefore create a
duplicate rather than overwrite an unrelated result.

## Checks

```sh
moon run pipeline:fmt
moon run pipeline:check
moon run pipeline:lint-src
moon run pipeline:test
moon ci --force
```

Integration tests require `TEST_DATABASE_URL`; they create isolated PostgreSQL
schemas and remove them afterward. Do not point tests or smoke checks at a
production database.

## Issue tracking

Beads stores project issues in its own Dolt database, separate from the
application's PostgreSQL database. Synchronize issue changes with `bd dolt push`
and retrieve remote changes with `bd dolt pull`. The configured DoltHub remote
is `priorlewis43/track-scrapper-v2`.
