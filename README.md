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
- Chromium or Chrome installed and discoverable on `PATH` for browser transport.
  Chromiumoxide launches a dedicated headless profile by default. Browser pages
  execute their JavaScript; normal certificate verification remains enabled.
  An optional transport `driver_url` is a Chromium DevTools HTTP or WebSocket
  endpoint, not a WebDriver endpoint; externally managed browsers retain their
  own profile and are not shut down by the scraper.
  Captured source fixtures retain their original third-party HTML and scripts.

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
Large worksheets continue as `Athletes_2`, `Issues_2`, or
`Athlete_<id>_2` before exceeding Excel row or hyperlink limits. Source URLs
remain clickable; every continuation sheet is reopened and checked before
publication. The configured athlete-row file splitting still applies.

The `Athletes` sheet has exactly one row per canonical athlete ID in the selected
known cohort/state. Each comparable event/category gets PR columns on that row;
indoor, outdoor and XC marks are not collapsed together. A review flag does not
hide an otherwise selected athlete: `needs_review` is explicit on the row and
the conflict is also listed in `Issues`. Unknown graduation years remain unknown
and are reported in `Issues`, not assigned to the requested cohort.

MileSplit profiles retain all public sport/competition-level PRs, visible career
results, biography/identity sections, JSON-LD, published progression values,
links and media metadata. `Profile_details` contains the exact retained JSON in
ordered UTF-8-safe chunks, without multiplying rows in `Athletes`. Masked results
and subscription examples are not invented career performances.

Before parsing a MileSplit athlete response, the CLI retains its
transport-sanitized bytes under their SHA-256 content address. The default
directory is `${XDG_DATA_HOME:-$HOME/.local/share}/track-scrapper-v2/evidence`;
`TRACK_SCRAPPER_EVIDENCE_DIR` can select another absolute path. Per-page database
metadata records the artifact, hash, byte length, effective URL, status and
retention time. Replay reuses identical bytes and refuses to overwrite corrupt
evidence. An evidence-write failure leaves the page failed, not successful.


Import the published CSV from
[`rust-athletic-school-addresses`](https://github.com/lprior-repo/rust-athletic-school-addresses)
using a SHA-256 obtained independently for the exact file:

```sh
cargo run -- scrape --school-addresses /path/to/high_school_addresses.csv \
  --school-addresses-sha256 EXPECTED_SHA256
cargo run -- scrape --download-school-addresses /path/to/new-addresses.csv \
  --school-addresses-sha256 EXPECTED_SHA256
```

The first form is offline; the second explicitly downloads the repository's
`output/all_us_high_schools.csv` over HTTPS and refuses to overwrite the
destination. Both verify the supplied digest before database changes. The
downloader is pinned to repository commit
`b7058732f05ab59105aa042e66d8a8cad467bf3c`; its CSV SHA-256 is
`e141d147329289f957dbc160ce6b765a844754ba32ecf970697398043fa2c0ee`.
A digest provided by the caller is not an independent signature. The importer accepts
the published `high_school_addresses.csv` and `all_us_high_schools.csv`
schemas, requires unique twelve-digit NCES IDs and `NCES_CSV` rows, and adds
schools under the NCES source without merging them with athletic-provider
identities. It preserves the exact NCES ID, supplied artifact path and hash,
source, coordinates and reported validation/geocode fields as provenance.
Downloaded records retain the pinned GitHub URL as `source_url`; local imports
use the supplied filesystem path there, and both record the local artifact path.
Address type is unspecified by the CSV, not assumed physical or mailing.
Despite the file names the dataset contains schools of multiple grade levels.
Existing NCES rows are not overwritten on reimport. Existing athletic-provider
schools can only be enriched using the conservative corroborated match described
below; no athlete or school identity is joined by name. Import and enrichment
commit together in one transaction. No Python runtime is required.

The retained NCES PSS 2023–24 public-use CSV can also be imported offline:

```sh
cargo run -- scrape --private-school-addresses /path/to/pss2324_pu.csv \
  --school-addresses-sha256 EXPECTED_SHA256
```

PSS identities use `pss:<uppercase PPIN>`, preserving eight-character IDs and
leading zeros without colliding with public-school IDs. The published
`PADDRS`/`PCITY`/`PSTABB`/`PZIP`/`PZIP4` tuple is explicitly **mailing** data.
Physical-location columns never fill missing mailing components. Raw phone,
grade range, enrollment, coordinates, source hash and school year remain in
provenance. Existing rows and UUIDs remain unchanged on replay. All grade levels
are retained; school-directory membership alone does not establish an athlete's
school affiliation.

Every existing and new school receives a unique, persistent PostgreSQL UUID.
Existing numeric foreign keys stay intact. The spreadsheet includes canonical
school UUIDs; an imported NCES row and an unlinked athletics-site row retain
separate UUIDs until reliable identity evidence establishes that they are the
same school. PostgreSQL 13 or newer is required for `gen_random_uuid()`.

To enrich already-observed schools without contacting providers, supply both a
local reference directory CSV and its manifest:

```sh
cargo run -- scrape --school-directory /path/to/school_directory.csv \
  --school-directory-manifest /path/to/manifest.json
```

This mode does not start a crawl or require a scraper configuration. It checks
the CSV's SHA-256 and byte length against the supplied manifest before opening
the database. The manifest is not an independently authenticated signature;
choose trusted inputs. It matches only unique normalized name, state and city
tuples on both sides, corroborated by an exact existing street address or a
matching website unique across both datasets. Missing, ambiguous or
uncorroborated identities are not linked. Conflicting existing addresses or
postal codes are preserved and counted in the command output. Nonempty facts
are never overwritten. The update runs in one transaction, and provenance for
each filled field records the directory row key, row sources, artifact and
manifest hashes, generation and manifest input lane hashes in `schools`.
The flattened directory does not identify whether an address is physical or
mailing, so the stored provenance explicitly records its type as unspecified;
it must not be interpreted as a verified physical address. A later source
observation replacing an imported field removes that field's provenance.
The input artifact may itself combine physical and mailing addresses; no
components are borrowed from another directory row.

Browser mode uses a fresh tab per target and reuses the browser profile for
session cookies. When a target supplies a browser origin, it opens that page
before issuing the GET or POST in its browser context. Source requests are paced
at no more than one per second, including the origin navigation; page-initiated
subresources and JavaScript requests are not individually rate-limited. Browser
responses are limited to 16 MiB, requests have timeouts, and access challenges
stop acquisition for human handling rather than being bypassed.

The crawl reports an error if `limits.max_pages` is exhausted while checkpoints
remain pending. Increase the limit if needed and run the scrape again to resume
from its persisted checkpoints.
Nonblocked page failures remain recorded while untouched pages continue. The
scrape reports exhausted failures when runnable work is drained; `--refresh`
does not reset failed or interrupted attempt counts.

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
MileSplit HY-TEK results with `Year` and `Finals` columns are retained, including
the reported school text when no school identifier is published. Team-score
sections are not interpreted as individual results.
Published manual and spreadsheet-style result tables are retained as unassigned
results when no source athlete identifier is available. When the published
canonical file matches the selected result, its rows are not queued twice.
AthleticRUNMEET tables retain individual and relay rows, including DNS/DQ and
duplicate published rows, without inventing athlete IDs. Within each source's
eight scheduling turns, six reservations go to profiles, one to rosters and one
to bulk pages; empty classes fall back to available work. This prevents the
national roster backlog from starving already-discovered athlete profiles.

MileSplit's public structured performance response supplies athlete identities
only when the athlete ID agrees with a published HTTPS profile URL and the row
belongs to the requested meet and a published result file. Rejected ownership
and relay rows remain unassigned. The captured response has no verified published
total; a successful acquisition does not establish complete meet coverage.
Replay a captured structured response with `--kind results`, `--subject-id` for
the meet, `--sport`, and one `--result-set-id` per published public result file.
MileSplit's explicit unknown graduation markers, including `0`, remain unknown.
Unsupported structured graduation claims remain unset and flag the athlete for
review; the claim and original source JSON remain available. A separately
published failure status does not replace the original mark and cannot win a PR.

## Backup and recovery

PostgreSQL is the durable authority. Normal process-crash recovery uses its WAL;
do not replace the data directory or reset checkpoints to recover a stopped
scraper. The exercised server has `fsync`, `full_page_writes` and
`synchronous_commit` enabled. WAL archiving is disabled, so this setup does not
provide point-in-time recovery after loss of the data directory.

A complete **application** backup consists of a PostgreSQL custom-format dump
of `public` plus the retained evidence directory. It includes all six application
tables, UUIDs, source mappings, result history, attempts/checkpoints and SQLx
migration history. Role passwords and cluster configuration are not included.
With an existing private backup directory:

```sh
pg_dump --dbname "$DATABASE_URL" --format=custom --schema=public \
  --no-owner --no-privileges --file "$BACKUP/database.dump"
tar --create --gzip --file "$BACKUP/evidence.tar.gz" \
  --directory "$HOME/.local/share/track-scrapper-v2" evidence
```

Adjust the archive directory for a custom XDG/evidence path. Retain a SHA-256
manifest for both files. Copy the backup off the database disk for disk-loss
protection. Older pages fetched before evidence retention was introduced do not
gain original response bytes retroactively.

Restore only into a newly created, isolated database. The following exercised
restore command removes matching objects in that **restore target**, never the
live database:

```sh
pg_restore --dbname "$RESTORE_DATABASE_URL" --no-owner --no-privileges \
  --single-transaction --exit-on-error --clean --if-exists "$BACKUP/database.dump"
tar --extract --gzip --keep-old-files --file "$BACKUP/evidence.tar.gz" \
  --directory "$RESTORE_APPLICATION_DATA"
```

Recreate the application login with the required database permissions separately.
Restore evidence at its recorded location, or locate backed-up bodies by their
recorded SHA-256 in the configured evidence root. Re-run `scrape` against the
restored database to continue unfinished checkpoints; `export` does not scrape.
The recovery drill compares row counts and ordered full-row SHA-256 fingerprints
for all six tables and migration history, then exercises the restored CLI.
A verified snapshot recovers its captured state, not subsequent writes.

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
