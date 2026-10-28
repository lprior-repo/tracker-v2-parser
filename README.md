# Track scrapper v2

A Rust CLI for collecting published athletics data into PostgreSQL and exporting
observed, comparable results to XLSX. The reference `athletic-rust-pipeline` uses
Fjall and Restate; this application uses neither for its runtime storage or
orchestration.

## Prerequisites

- PostgreSQL and a database URL with migration permission for scraping/imports.
  Export requires SELECT access to a compatible schema and `_sqlx_migrations`;
  it does not apply migrations or repair the database.
- The Rust toolchain pinned in `rust-toolchain.toml`.
- Moon 2.2.4 for the project tasks.
- For integration tests, `TEST_DATABASE_URL` pointing to a disposable PostgreSQL
  database where the test user may create and drop schemas.
- Chromium or Chrome installed and discoverable on `PATH`. Chromiumoxide launches
  locally owned Chromium and an isolated acquisition context. Documents execute
  their JavaScript; certificate verification remains enabled. External
  `driver_url` endpoints are rejected because their network and cleanup ownership
  cannot be established. Other browser contexts are not used.
  Fixture provenance records representative third-party markup and sanitization.

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
The checked-in configuration starts with Wisconsin (`WI`), graduation year 2027,
season 2026, a 1,000-page checkpoint budget, and MileSplit enabled. Add states to the `states` array after
checking the Wisconsin acquisition/export loop. Athletic.net is disabled in this
starting configuration; enable it only when ordinary public access is available.
Changing states restricts resumed checkpoint acquisition and unfinished-work
reporting without deleting or resetting saved data from other states. Unknown
checkpoint scope is not permission to start an unrelated national crawl.
Provider-wide access stops still require human review.
Graduation-year filtering skips athlete-profile checkpoints already known to
belong to another cohort without changing their saved status or attempts.
Profiles with unknown graduation years remain eligible for discovery.


Scraping uses bounded attempts and checkpoints; access challenges require human
handling. MileSplit requests matching the observed robots exclusions
(`/rankings`, `/virtual-meets`, `/api/`, and `/contact`) stop before acquisition,
including redirect and browser subresource routes. The restriction is applied
conservatively across MileSplit subdomains; encoded or ambiguous path forms
cannot bypass it. The evidence is the `User-agent: *` section of
`https://www.milesplit.com/robots.txt`, reviewed on 2026-10-28. This narrow denial
guard is not a complete robots interpreter, a freshness check, or permission to
crawl other paths or origins. Host-specific policy review and existing access
stops remain binding. Parser-discovered API targets remain evidence/checkpoints;
they are not silently discarded or acquired despite the restriction.
 Export reads one repeatable-read, read-only snapshot without scraping
or migrating. It stages and reopens new XLSX files before non-overwriting
publication. A completion manifest is published last, listing every part, byte
length and SHA-256. An absent manifest or mismatched part is not a completed set.
The collecting `write_spreadsheets` and `write_spreadsheets_with_issues` APIs
also publish a manifest for empty, single-part and split exports. Their returned
paths contain only workbooks; the manifest is beside the requested output with
the extension `manifest.json`.

`Database::athlete_records` and `athlete_records_for` use the same bounded
collector as `export_snapshot`, returning only its athlete records. Collection
limits include coverage issues even though these two convenience APIs discard
them: at most 10,000 combined rows and 32 MiB of serialized content, plus the
snapshot's per-athlete/component limits. Exceeding a limit returns
`ExportLimitExceeded`, never a partial result. Use `begin_export_snapshot` for
bounded batches or `write_export` for larger exports. These reads use a
read-only transaction without migrations; use `connect_for_export` to connect
without migrating the existing schema.

Ordinary rollback atomically claims this invocation's visible entries before
checking ownership. Concurrent replacements are restored without overwriting a
newer destination; if restoration conflicts, the unrelated entry is retained at
the recovery path reported by the error. Abrupt process death can leave
unmanifested partial files, which are not automatically deleted. Choose a new
output name rather than overwriting another file.

The checked-in configuration selects `layout = "single_sheet"` under `[export]`.
This writes exactly one workbook with one `Athletes` worksheet across database
batches: school/address, published track/XC coach names and emails, profile links,
and every captured source-published PR. PR columns retain the published sport, event, competition level and provider;
all-level and middle-school marks remain visible with their own labels. Raw event
labels and marks remain visible even when numeric normalization is unsupported.
Repeated claims within a column are sorted deterministically and all retained.
Public all-season headline PRs retain their published attribution. Ordinary
results also provide event bests within compatible timing, wind, event and course
contexts; they do not compete across levels or incompatible contexts. The athlete
cohort/state settings still apply; this layout includes only confirmed selected
graduation years. School addresses require a published street address and retain
postal order. No JSON, review prose or supporting sheets are written. Compact
prepared cells are retained across batches; raw profile JSON and result records are discarded after each batch. The total athlete count must fit
`rows_per_file` and Excel's row capacity; prepared storage is limited to 128 MiB.
Exceeding a row, byte, cell or column capacity fails explicitly without publishing
a complete export or adding overflow tabs. Empty issue-only batches do not create
extra empty files.

`layout = "detailed"` selects the diagnostic export described below.
The main sheet has one row per selected canonical athlete ID. Only explicitly
published high-school observations compete for PRs. All-level, middle-school,
college, unknown-level and unconfirmed prior-revision observations remain in
`Observations` and `Issues`, but cannot win. Graduation year, grade and school
membership never manufacture HS eligibility. Indoor, outdoor, XC, timing, wind,
implement and competition-category distinctions remain separate.

Current source-profile school claims supply the main school's address and
contacts; historical represented schools stay attached to results. Unknown
current affiliation remains blank and flagged. Unknown cohorts or school states
remain visible review candidates, not accepted scope membership; known
out-of-scope records remain excluded.
State selection trims surrounding Unicode whitespace and ignores ASCII case
through both database selection and workbook writing.

Main rows use at most 5,455 stable PR keys; further keys and overlong comparison
keys remain in `PR_overflow`. Review summaries are limited to 1,024 UTF-16 units
with explicit continuation notices. Complete issues use continuation rows and
coverage issues occur once across the completed workbook set. Other oversized
cells use `Cell_details`. Worksheets split before Excel row, column, cell or
hyperlink limits. `Observations` is always present, independently of the optional
per-athlete worksheets.

Published coach names appear in `coach_names`, independently of `coach_emails`.
A named track/XC coach remains visible when no mailbox is published; an
unambiguously associated mailbox may remain without a published name. Missing
coach names or mailboxes are explicit review issues even when another coach has
that fact. Missing emails stay blank, never guessed or inferred from names.
District-wide or otherwise unconfirmed school associations and unknown current
tenure remain explicit contact review notes, not accepted affiliations. Replaying
a contacts page retains earlier claims but flags those not confirmed by the new
parse; unchanged current claims keep their identity and clear that earlier-claim
flag. Names and old mailboxes remain visible with their warnings rather than
being silently discarded or presented as verified current contacts. Replaying a
completed contact capture does not consume another automatic attempt, and an
existing access stop for that contact authority still prevents replay.


MileSplit profiles retain public sport/competition-level PRs, visible career
results, biography/identity sections, JSON-LD, progression values, links and media
metadata. `Profile_details` contains sanitized public JSON in ordered chunks and
one supporting source URL per profile, without multiplying athlete rows.
Conflicting schema/progression category claims are retained and flagged on the
first parse. Masked cells and subscription examples never become performances.
Only explicit provenance-link columns become hyperlinks; JSON remains literal.


Before parsing a MileSplit athlete response, the CLI retains its
transport-sanitized bytes under their SHA-256 content address. The default
directory is `${XDG_DATA_HOME:-$HOME/.local/share}/track-scrapper-v2/evidence`;
`TRACK_SCRAPPER_EVIDENCE_DIR` can select another absolute path. Per-page database
metadata records the artifact, hash, byte length, effective URL, status and
retention time. Replay reuses identical bytes and refuses to overwrite corrupt
evidence. An evidence-write failure leaves the page failed, not successful.
Unsupported sensitive JavaScript expressions cause their entire script body to
be removed before retention, followed by another complete redaction pass.
Public DOM and separate athletic JSON remain available; unsupported sensitive
syntax outside safely located scripts fails the capture instead of retaining
partial secrets. Retained evidence is sanitized, not an archival original.


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
Same-ID NCES corrections update only fields whose recorded import owner and exact
prior value still match. Sparse reimports preserve known values; manual and
unverifiable legacy conflicts remain unchanged and are reported. Stable IDs and
UUIDs survive replay. Import and enrichment commit in one transaction, without
name-only identity joins or a Python runtime.

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
provenance. UUIDs remain stable; corrected import-owned facts follow the same
guarded update policy. All grade levels are retained; directory membership alone
does not establish an athlete's school affiliation.

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
the database. Duplicate `(sources, key)` identities reject the entire directory
before database access; the same key in distinct source namespaces stays separate.
The manifest is not an independently authenticated signature; choose trusted
inputs. Initial enrichment requires unique normalized name,
state and city plus a corroborating street address or unique matching website.
Later corrections follow the full saved `(source, source_id)` identity even when
its name/city changes, but only still-owned fields may change. Manual or ambiguous
conflicts remain intact and counted. Each imported field records its source identity,
exact saved value, artifact/hash and address kind. CSV input is limited to
128 MiB and 250,000 rows; a directory manifest is limited to 1 MiB. Accepted
imports still retain parsed rows and indexes in memory.
The flattened directory does not identify whether an address is physical or
mailing, so the stored provenance explicitly records its type as unspecified;
it must not be interpreted as a verified physical address. A later source
observation replacing an imported field removes that field's provenance.
The input artifact may itself combine physical and mailing addresses; no
components are borrowed from another directory row.

Browser acquisition uses fresh tabs in one owned context, retaining cookies
within the running session. HTTP requests are paused and fulfilled through
validated, public-DNS-pinned connections before redirects or subresources reach
their destinations. Native transports have a nonforwarding local sink; auxiliary
frames, workers, sockets and popups are restricted and attempted use is reported
as incomplete. Caller credentials remain origin-bound. Page requests, bodies,
queued work, concurrency and cleanup have finite limits. Challenges are not
bypassed, and failed/cancelled acquisition does not establish complete coverage.

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
Legacy raw/table reconciliation admits at most 10,000 candidate observations per
meet set. Already file-identified observations are excluded before that budget,
so current-format results in other files do not block a small file replay.
MileSplit HY-TEK results with `Year` and `Finals` columns are retained, including
the reported school text when no school identifier is published. Team-score
sections are not interpreted as individual results.
Published manual and spreadsheet-style result tables are retained as unassigned
results when no source athlete identifier is available. When the published
canonical file matches the selected result, its rows are not queued twice.
AthleticRUNMEET tables retain individual and relay rows, including DNS/DQ and
duplicate published rows, without inventing athlete IDs. Within each source's
eight scheduling turns, five reservations go to profiles, one to rosters, one to
official-school contacts and one to bulk pages; empty classes fall back to
available work. This prevents roster/bulk backlogs from starving profiles or
contact discovery.

Export staging reads at most 1,000 athletes and 1,000 coverage issues per batch,
with 8 MiB serialized budgets per category. An athlete may have at most 10,000
observations; constituent rows and a 32 MiB constituent budget are checked before
aggregation. A single oversized record fails explicitly rather than disappearing
from the output. The collecting API also has a 32 MiB/10,000-item total cap; the
CLI streams batches. These are application admission limits, not a PostgreSQL
peak-memory guarantee: PostgreSQL can materialize one stored JSONB/varlena value
before measuring it. At most 10,000 workbook parts can be staged.
Coverage is read in bounded status/note/unassigned phases, filling batches across
phase boundaries; exact page-URL lookups use a crash-safe PostgreSQL hash index.
Unfinished contact checkpoints have a partial authority index so propagating a
blocked site's stop does not scan the national page backlog.


Discover published Wisconsin WIAA school contacts after collecting provider
schools, with `WI` selected and contact collection enabled:

```sh
cargo run -- scrape --config config.toml --wiaa-directory
cargo run -- scrape --config config.toml
cargo run -- export --config config.toml --output athletes-with-contacts.xlsx
```

The first command reads the public WIAA directory and enqueues school-detail
pages matched by exact normalized school name, Wisconsin state and city. It
reports missing, uncorroborated and ambiguous matches without guessing identities.
The subsequent scrape acquires those pages and persists published track/XC coach
names, emails and school addresses; discovery alone does not populate those
fields. Provider school IDs remain intact. Existing site access stops apply,
and WIAA global navigation is not crawled.

Scraping backfills contact checkpoints from existing in-scope athletic-provider
school websites and discovers them when new school pages are parsed. Plain
published DNS hostnames/paths are requested with HTTPS while the original
website string remains stored; ambiguous malformed addresses are not repaired.
A full-page access challenge blocks contact acquisition for that site's
authority, not unrelated school sites or athlete pages. Public sign-in links and
embedded contact-form CAPTCHA widgets do not by themselves block a page.
Staff links use the effective response URL and published HTML base without
changing checkpoint identity or broadening the school-site crawl authority.
Contact HTML is parsed in memory rather than retained as a raw capture;
recognized credential-bearing derived URLs are omitted before storage or
queueing, without discarding other published contact or postal records.
An initial GET contact URL returning HTTP 404 can discover its same-site root in
the same failure transaction; this does not reset the old page's consumed
attempts or override access stops. A previously completed root is not retried.
Office mailboxes require unambiguous matching school headings and visible
evidence; hidden links and stale hidden text do not create ambiguity.

Published role text and sport scope stay attached to each contact claim; names
are never inferred from mailbox spelling. Contacts are source claims, not
canonical people or assertions of current tenure. Their source URLs remain
stored as evidence, and capture/check time does not establish employment dates.

Migration `202610270005_contact_source_claims.sql` preserves existing contact
rows, permits null coach email, and backfills a 64-hex SHA-256 `source_key` from
PostgreSQL's UTF-8 JSONB array text of source URL, name, role, sport and normalized
email (`lower(btrim(email))`, including null). Uniqueness is now
`(school_id, kind, source_key)`: unchanged replay is idempotent, while different
names, sports or source URLs remain separate claims, including shared-mailbox
and no-email coaches. Office contacts remain distinct. Every row requires a
nonblank published name or valid email; malformed nonnull email is rejected.
Older migration files and their SQLx checksums are unchanged.

Unnamed linked roster observations remain in `Issues` with profile follow-ups;
named peers are saved without inventing names for the incomplete observations.

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

PostgreSQL stores the application data. DoltHub synchronizes Beads issues only;
it does not back up athletes, results, checkpoints, retained HTML, or workbooks.
GitHub carries the code and migrations, not the live database or credentials.

The portable migration tools require Python 3 and PostgreSQL command-line tools.
Use the same PostgreSQL major version on both machines. A backup captures all six
application tables and SQLx migration history from one exported database snapshot,
plus retained evidence, the selected configuration, and saved exports. It does
not copy role passwords, `.env`, browser profiles, old backup directories, or
unrelated Downloads. Backup and restore share the same credential-filename
rejection policy. Original evidence paths remain provenance; portable evidence
files are named by SHA-256.

### Package on the source machine

Use libpq environment variables or a private `PGPASSFILE` for authentication;
never put a password or database URL in command arguments. Stop source writers
before the final cutover so the old and new machines cannot diverge afterward.
The dump and logical fingerprints use one snapshot even if writers are active.

```sh
PGHOST=localhost PGUSER=track_scrapper \
python3 scripts/migration/backup.py \
  --database track_scrapper \
  --data-dir "$HOME/.local/share/track-scrapper-v2" \
  --config config.toml \
  --output "$HOME/track-scrapper-migration.tar.gz"
```

For exports outside the application's `exports` directory, repeat `--export-file`
for their completion manifest and **every** workbook part. If a recognized
completion manifest exists beside a selected workbook, it must be selected too;
both exact workbook stems and numbered-part manifests are checked. Missing parts,
incomplete metadata and checksum mismatches are rejected. Only workbooks without
applicable completion metadata are treated as legacy files, not certified exports.
The archive has a per-file SHA-256 manifest and full-row database fingerprints.
Existing output files are never overwritten.

### Transfer privately

The archive contains database records and source evidence. Keep it out of Git
and public DoltHub repositories. Tailscale Taildrop encrypts the transfer between
your devices; the archive is permission-restricted but **not encrypted at rest**.
Use an encrypted disk or separately encrypt the archive if it will be stored on
an untrusted disk or cloud service. Checksums detect corruption, not a maliciously
replaced manifest: restore only a bundle from your trusted source machine.

```sh
sha256sum "$HOME/track-scrapper-migration.tar.gz"
tailscale file cp "$HOME/track-scrapper-migration.tar.gz" lewiss-mac-mini:
```

Receive the file in the Mac's Tailscale app. Compare `shasum -a 256` against the
source checksum before restoring. Do not expose PostgreSQL's port to the internet
or copy its live data directory between Linux and macOS.

### Hydrate a new machine

Fetch this repository's `main` first. On macOS, Python 3 and Homebrew must already
be available. `--bootstrap-macos` installs and starts the matching PostgreSQL
Homebrew service; omit it on Linux where PostgreSQL is already running.

```sh
git pull --ff-only origin main
python3 scripts/migration/restore.py \
  --archive "$HOME/Downloads/track-scrapper-migration.tar.gz" \
  --database track_scrapper \
  --data-dir "$HOME/.local/share/track-scrapper-v2" \
  --bootstrap-macos
```

Use a role allowed to create the new database, not a PostgreSQL superuser when
restoring an external dump. Dumps execute SQL; archive validation is not a SQL
sandbox. The restore refuses an existing database or data directory. It checks
paths, file types, checksums, PostgreSQL major version, and all logical row
fingerprints before publishing the restored files. It never cleans or drops an
existing database. A failed restore identifies any newly created partial target.

The new data directory contains `config.toml`, `evidence`, `exports`, and a private
`.env` configured for the destination. Build the application from the repository
root, then run it from that data directory so its dotenv loader reads the
destination settings:

```sh
REPO="$PWD"
env -u CI moon run pipeline:build
(
  cd "$HOME/.local/share/track-scrapper-v2"
  env -u DATABASE_URL -u TRACK_SCRAPPER_EVIDENCE_DIR \
    "$REPO/target/release/track-scrapper-v2" export \
    --config config.toml --output exports/restored-athletes.xlsx
)
```

Reopen the export and check its completion manifest before resuming scraping.
Restoration preserves captured data; it does not fill missing PRs or contacts or
prove complete source coverage. Keep the old machine unchanged until the new
database and files have been verified. Older captures without retained response
bodies do not gain original bytes retroactively.

PostgreSQL WAL protects ordinary process-crash recovery. This logical backup does
not include role passwords, cluster configuration, or point-in-time recovery.
Beads issues move separately using `bd dolt push` on the source and `bd dolt pull`
on the destination.

## Checks

### Native macOS

Use native Apple Silicon tools on an ARM64 Mac. Chrome can be installed as a
macOS application; it need not have a `chrome` command on `PATH` when the browser
discovery library finds `/Applications/Google Chrome.app`. Keep its sandbox
enabled and let the application create its own temporary browser profile.
Build from source with the pinned toolchain; do not copy Linux executables,
`target` directories, profiles, or systemd launch wrappers.

Install PostgreSQL through Homebrew if needed. For a migration, first discover
the source server's major version and install the matching formula. A private
local cluster on a separate loopback port can keep this application independent
of other services. Provision separate application and disposable test databases,
with non-superuser login roles. Provision the read-only NOLOGIN test role and
grant membership as described below, without granting the test user CREATEROLE.
Keep destination credentials in private local settings; never copy Linux `.env`.

Bound compiler and test resources when running the documented checks:

```sh
CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=2 MOON_CONCURRENCY=1 moon ci --force
```

Set `TEST_DATABASE_URL` and `TEST_EXPORT_READONLY_ROLE` privately before that
command. The application is an on-demand CLI, not an HTTP service. Start its
database before invoking `scrape --input` for offline replay or `export`, and
stop only its own database cluster when finished. Replay a representative fixture
twice, verify stable identities/results/checkpoints, then export and reopen the
workbook and verify every manifest part's byte length and SHA-256.

macOS temporary-directory paths can contain a `/var` symlink. Migration backup
inputs must use canonical, existing paths: source symlinks and symlinked parents
are intentionally rejected. Test fixture roots are canonicalized before creating
their contents so security assertions exercise the selected path itself.

A fresh database does not restore the real dataset. Durable source access stops
apply across machines and databases. Keep Athletic.net disabled and retain the
Wisconsin scope, one request per second, and 1,000-page budget.

For an explicitly requested fresh collection of public results, an operator can
select this acquisition policy:

```toml
[acquisition]
mode = "public_results"
reviewed_media_stops = ["https://mn.milesplit.com/athletes/13557507-tana-borland/photos"]
collect_contacts = true
```

Normal public-results runs prioritize saved school chains before broad
discovery: team, roster, matching athlete profile and published result sections.

To finish the queued school chains in the selected state and graduation cohort,
add `profile_coverage_only = true` to `[acquisition]`. This
optional setting defaults to false and requires MileSplit enabled with
Athletic.net disabled. Focused runs complete queued team and roster parents,
then the matching athletes' base profiles and published stats/progression
sections within those school chains, then saved school contacts when
`collect_contacts = true`. A queued roster can discover additional matching
athletes before the crawler advances to the next school. Focused runs add no
directory seeds or contact backfill and do not expand into media, articles or
broad meet discovery. Optional contact requests that exhaust their attempt
budget retain their failure details and allow the next school to proceed;
unfinished team or athlete stages still prevent a false completion.
Roster profiles with no graduation year receive a base-page check to establish
their year. Result follow-ups and the recruiting worksheet require the selected
graduation year; known nonmatching athletes remain outside that work.
The configured rate (at most one request per second), page budget and access-stop
guards still apply. Profiles already fetched
without public marks do not gain invented PRs; this mode does not establish
statewide completeness.

`collect_contacts = true` enables the existing school-contact parser/backfill
while retaining authority-scoped contact access stops. Its default is false.
The listed stop was reviewed as subscription photos. Its blocked checkpoint is
retained and never requested again by this policy. Only exact canonical athlete
photo/video URLs may be reviewed this way. Public-results mode excludes media,
external school-contact acquisition unless enabled, and published robots restrictions before
making requests. Local exclusions retain a separate marker and consume no
attempts. Existing attempted exclusions require review. Any other blocked
checkpoint still stops its source; new HTTP denials, challenges, login demands,
and paywalls retain the existing immediate stop behavior. Omitting this policy
retains the original complete acquisition behavior. Preserve source-stop evidence
and disclose any unavailable source inventory when starting fresh.

### Canonical gate

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
The read-only export scenario requires either permission to create a temporary
test role or a preprovisioned NOLOGIN role named by `TEST_EXPORT_READONLY_ROLE`.
Grant the test user membership in that role, not `CREATEROLE`; each scenario
grants/revokes SELECT access only to its disposable schema. On this workstation:

```sh
TEST_EXPORT_READONLY_ROLE=track_scrapper_export_readonly moon ci --force
```


## Issue tracking

Beads stores project issues in its own Dolt database, separate from the
application's PostgreSQL database. Synchronize issue changes with `bd dolt push`
and retrieve remote changes with `bd dolt pull`. The configured DoltHub remote
is `priorlewis43/track-scrapper-v2`.
