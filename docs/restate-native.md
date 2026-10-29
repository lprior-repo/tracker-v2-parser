# Native durable pipeline

The production entry point is the Restate workflow. PostgreSQL remains the
business database. Restate owns execution, timers and continuation; PostgreSQL
holds source checkpoints, cumulative attempt history, wire admission and atomic
effect receipts. Captures and export generations remain in a private artifact
directory. No Docker runtime is required.

The qualified versions are native Restate **1.8.1** and Rust SDK **0.13.0**.
The SDK dependency is pinned exactly. Use this repository's dated Rust toolchain;
the SDK requires Rust 1.90 or later. The reference implementation is
[`athletic-rust-pipeline` at bf647f0](https://github.com/lprior-repo/athletic-rust-pipeline/tree/bf647f0c2ffc55a70c8647c4a0ba4c09fb0b08ef).
Its attempt, receipt and explicit outcome patterns are adapted to PostgreSQL.
Fjall storage and experimental server features are not imported.

Prepared single-sheet rows use a private bounded disk spool (8 GiB total, 8 MiB per row, 128 MiB per incoming batch). Rendering streams those rows and verification reloads one immutable prepared chunk at a time, comparing every worksheet cell through a streaming reader. Frozen export aggregates and workbook artifacts are also bounded at 8 GiB. Workbook hashing and verified delivery copy streams; generic JSON/input artifacts retain the 64 MiB memory limit. A failure removes the owned spool and preserves the last verified published workbook.

## Execution and recovery

`AthleticCrawl` executes bounded page batches, invokes a verified export and
schedules the next batch durably. The school/profile phase finishes before the
WIAA directory refresh and general results phase. Source/configuration identity
is immutable within a phase. Each phase has a separate operation generation.
The configured page budget applies to each batch; the complete command has an
explicit deadline of at most 24 hours and a bounded batch count. Deadline or
budget exhaustion is visible and preserves checkpoints for another command.
The production operator can select a 2,000-page batch to reduce export pauses;
this is a deployment setting, and the checked-in configuration remains unchanged.
Transient infrastructure outages retry without an AI process; a source access
stop requires human handling.

Durable crawls support one to three page slots. With `concurrency = 2` or `3`,
each window journals page selection and reservation before eagerly sending its
children, then attaches every child in submission order before proceeding.
Current-window IDs exclude duplicate pages, school-parent overlap and concurrent
sections of the same athlete. Normal selection still resumes an interrupted
reservation. Run one crawl owner per database; independent workflow generations
are not a mechanism for increasing the configured concurrency.

Parallel acquisition uses shared PostgreSQL slots and retains the exclusive
maintenance/import lock. At most one browser acquisition runs at a time. HTTP
request starts share provider and origin limits across slots and worker restarts;
an abandoned request retains its full reservation debt. The shared provider/origin
lease waits for response headers before admitting another request. Parallel mode
removes the extra post-header cooldown when the start interval has already elapsed.
`contact_requests_per_second` explicitly overrides the conservative contact
defaults and is capped by `requests_per_second`, with a maximum of three.
Omitting the option preserves existing configuration identities and contact
defaults. Source stops are checked again after admission. These limits are
ceilings; network latency, browser work and exports can reduce observed throughput.

`AthleticPage` records reservation, fetch/capture, parse and atomic persistence
as separate Restate steps. Capture and parse blobs are bounded, fsynced and hash
checked. A capture manifest can recover the interval between retaining bytes
and receiving PostgreSQL acknowledgement. Known denials have a separate small
stop manifest, so loss of the database acknowledgement cannot turn a denial
into another request.

Source HTTP requests execute inside recorded acquisition steps. Every physical
HTTP request, redirect and browser subresource additionally crosses the shared
PostgreSQL provider/origin pacing gate and wire intent/status ledger. Automatic
protocol retries are disabled for these clients. A wire intent without a
retained page capture is ambiguous: it consumes its attempt and cannot send
again under that same attempt. It can continue through an explicitly recorded
new attempt/cooldown, within the finite source budget and deadline. This is not
a claim that an external HTTP server executes exactly once. Individual response
bodies are recovered through the page capture, rather than separate Restate
journal entries for every browser resource.

DNS/connect/selected HTTP failures have three-attempt bursts and durable
cooldowns. Counters increase across recovery cycles. Rejected inputs and known
access restrictions do not receive an automatic retry or alternate IP/transport.
Untouched pages in the current workflow scope precede due retries; retry rounds
rotate before school order so a failing earlier school cannot stall later work.
Public reference CSV downloads use the same durable wire rules with a separate
three-chain budget and approved redirects. Production imports verify hashes and
commit their domain writes and operation receipt in one PostgreSQL transaction.

## Spreadsheet durability

An export freezes a database snapshot into immutable Athlete-ID chunks and
completes a manifest before rendering. Completed row chunks are reused after
interruption. If the initial freeze was incomplete, a new recorded freeze
generation starts; old partial chunks remain evidence and are not mixed into
the new snapshot. XLSX assembly may restart using the completed prepared chunks.

Verification compares every `Athletes` cell with frozen rows. With
`[export] review_sheet = true`, the same database snapshot freezes failed/blocked
checkpoint rows for a second `Review` sheet, including durable state and retry UTC.
Every Review header/cell is also verified before publication; zero failures still
produces its header row. The default remains a single Athletes sheet. Review is
bounded to 10,000 rows and 16 MiB with explicit cell limits, excludes unrelated
states, and includes unscoped discovery roots and existing disabled-provider stops.
URLs/errors omit credentials and raw response payloads. Deferred rows advise
waiting for automatic retry; blocked rows require human handling and prohibit bypass.
The Review presentation flag does not change acquisition identity or retry counters;
full export operation bindings still distinguish the selected workbook layout.
Only a verified workbook is published. An atomic canonical JSON pointer selects
the immutable generation. Optional `--published-workbook` delivery updates a
fixed local XLSX copy and manifest copy under the same publication lock. Replayed
older deliveries cannot replace a newer generation. The prior verified workbook
remains readable while the next generation is prepared. Editing a delivered workbook leaves the
immutable source generation intact.

## Native setup

Install the publisher's native server/CLI archive for the machine architecture
and verify its published SHA-256. Keep the complete server state on persistent
local storage. Bind the server, ingress, administration and worker to loopback.
Use fixed node/cluster names and preserve them with the data directory.

The installed machine uses ingress `127.0.0.1:18095`, admin `127.0.0.1:19095`,
fabric `127.0.0.1:15152`, and application worker `127.0.0.1:9089`. Both the server
and worker run as user LaunchAgents with KeepAlive and restrictive permissions.
They resume at login independently of the chat. A sleeping or powered-off Mac
does not execute work; persisted work remains available after it returns.

Start the application worker with credentials supplied only through its private
environment file:

```sh
track-scrapper-v2 durable serve --bind 127.0.0.1:9085 \
  --artifacts /absolute/private/artifacts \
  --published-workbook /absolute/outputs/athletes.xlsx
restate deployments register http://127.0.0.1:9085 --yes
track-scrapper-v2 durable crawl --config profiles.toml \
  --generation wi-2027-unique-run --hours 24
track-scrapper-v2 durable status --key wi-2027-unique-run-000000
track-scrapper-v2 durable export --config config.toml --operation-id reviewed-export-1
```

Configure both `RESTATE_ADMIN_URL` and `RESTATE_INGRESS_URL` for the native CLI.
`durable submit --request file.json` accepts a saved, bounded `CrawlRequest`;
`durable operation --request file.json` accepts a hash-bound `OperationRequest`.
Keep IDs stable when retrying the same saved request. Use a fresh generation for
new work; never reuse an ID for changed input. Local import inputs must first be
staged with the library's `stage_input` or `stage_cooperative_manifest` helpers.
An artifact descriptor is exactly `{"path":"/absolute/worker-artifacts/inputs/hash.input",
"sha256":"ACTUAL_SHA256","bytes":1234}` with the actual staged values. Imports
use `{"operation_id":"unique-id","spec":{"kind":"...",...}}`: `public_schools`
takes `csv` and optional `origin`; `download_public_schools` takes
`expected_sha256`; `private_schools` takes `csv`; `school_directory` takes `csv`
and `manifest`; `wiaa_cooperatives` and `reviewed_identity` take `manifest`.
`offline_replay` takes `config`, a source `target` and a staged `capture` artifact.
These are operation JSON fields, not additional CLI flags. `durable contacts`
submits the `wiaa_directory` operation using the selected configuration.
Credentials, browser cookies and complete response bodies are not invocation
arguments.

`moon run pipeline:native-worker-install -- --binary /absolute/track-scrapper-v2
--runtime /absolute/private/runtime --secrets /absolute/private/native.env`
copies the built worker and Rust launcher into an immutable directory identified
by both executable hashes, writes a private configuration and stages a versioned LaunchAgent.
`--activate` starts it. New code needs a new endpoint/port while existing work
drains; do not overwrite an endpoint serving in-flight invocations. The compiled
CLI has no direct scrape/export/replay commands. Library APIs remain available
for isolated parser and persistence tests; native qualification runs through
Restate.

## Operations and verification

Before deploying export changes, qualify a representative database snapshot with
credentials in the private environment and a separate existing empty artifact
directory. Pause and drain acquisition for this comparison. The example reads
the database through the export-only path, writes an unprojected baseline and a
durable frozen export, and requires every cell in all configured worksheets to match, including Review when enabled:

```sh
moon run pipeline:qualify-export -- \
  --config config.toml --artifacts /absolute/private/empty-qualification \
  --operation-id unique-readonly-qualification
```

Frozen input keeps all typed facts, source IDs, profile URLs, coach programs,
levels, revision status and headline PR evidence. Duplicated biography/history
and whole-result HTML are omitted from this export input. PostgreSQL and retained
source captures preserve the original evidence. Artifact bounds remain enforced.

Health alone is insufficient. Check native server partition recovery, registered
deployments, invocation state, the source attempt ledger and the latest verified
workbook. Gracefully stop worker intake and drain owned operations before
maintenance. The worker owns blocking export tasks even when an SDK waiter is
cancelled, with a bounded drain and explicit failure reporting.

Back up PostgreSQL, retained artifacts and the complete Restate state/configuration
together. Stop/drain services before a filesystem copy. Restate partition
snapshots are not a complete application backup. Rehearse restoration with
separate directories and loopback ports; never restore over a running source.

The canonical repository gate remains `moon ci --force`. It includes real
PostgreSQL scenarios, local HTTP/browser policy fixtures, receipt rollback and
acknowledgement-loss recovery, monotonic retries, access-stop retention, pacing,
frozen export reuse and verified publication. Run the native protocol rehearsal:

```sh
moon run pipeline:native-recovery -- \
  --server /absolute/restate-server --worker /absolute/track-scrapper-v2 \
  --evidence /absolute/new-private-evidence-directory
```

It requires `TEST_DATABASE_URL`, creates and removes its own schema and processes,
exercises native crawl/submit/status commands against isolated legacy checkpoints,
proves workflow-key deduplication, an automatic second batch and bounded parallel
windows with seven unique children split across five-page and two-page batches,
rechecks both parallel reports after backup restoration, replays a local
fixture twice, accepts work while the worker is stopped, restarts
the native server with the same state, replaces the worker and verifies retained
invocation IDs, hashes and a reopened single-sheet XLSX. Its evidence explicitly
separates those protocol checks from native chunk recovery and provider-outage
scenarios. It never sends a source request to a public provider.

`moon run pipeline:native-cadence` compares completed-response cooldown with
parallel start spacing using an isolated PostgreSQL schema and a delayed local
HTTP fixture. It prints physical arrival gaps and request rates; fixture results
do not establish public-provider throughput.

Official references: [durable versioning](https://docs.restate.dev/services/versioning),
[configuration](https://docs.restate.dev/services/configuration),
[snapshots and backups](https://docs.restate.dev/server/snapshots),
[Rust SDK](https://docs.rs/restate-sdk/0.13.0/restate_sdk/),
[disabling protocol retries](https://docs.rs/reqwest/0.13.5/reqwest/retry/fn.never.html).

The native operator deployment sizes Restate memory pools explicitly: RocksDB
1 GiB, SQL introspection 256 MiB, Bifrost cache 128 MiB, and invoker 256 MiB.
These budgets total 1.625 GiB before process overhead. PostgreSQL, the worker,
and the owned browser are additional; RSS must be measured under load. This is
not an operating-system hard limit. See [Restate memory guidance](https://docs.restate.dev/server/memory).
Native recovery qualification exercises the same three non-RocksDB limits with
a smaller 256 MiB fixture RocksDB cache. An Operations invocation explicitly
allows ten minutes of inactivity and two minutes to abort, covering the measured
multi-minute streaming freeze without the default one-minute inactivity trigger.
