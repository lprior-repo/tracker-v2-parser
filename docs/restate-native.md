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

## Execution and recovery

`AthleticCrawl` executes bounded page batches, invokes a verified export and
schedules the next batch durably. The school/profile phase finishes before the
WIAA directory refresh and general results phase. Source/configuration identity
is immutable within a phase. Each phase has a separate operation generation.
The configured page budget applies to each batch; the complete command has an
explicit deadline of at most 24 hours and a bounded batch count. Deadline or
budget exhaustion is visible and preserves checkpoints for another command.
Transient infrastructure outages retry without an AI process; a source access
stop requires human handling.

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
Public reference CSV downloads use the same durable wire rules with a separate
three-chain budget and approved redirects. Production imports verify hashes and
commit their domain writes and operation receipt in one PostgreSQL transaction.

## Spreadsheet durability

An export freezes a database snapshot into immutable Athlete-ID chunks and
completes a manifest before rendering. Completed row chunks are reused after
interruption. If the initial freeze was incomplete, a new recorded freeze
generation starts; old partial chunks remain evidence and are not mixed into
the new snapshot. XLSX assembly may restart using the completed prepared chunks.

Verification compares the final single `Athletes` sheet with the frozen rows.
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
fabric `127.0.0.1:15152`, and application worker `127.0.0.1:9085`. Both the server
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
Credentials, browser cookies and complete response bodies are not invocation
arguments.

`scripts/restate/install_native_worker.py` copies a built binary into an immutable
hash-named directory, writes a private launcher and stages a versioned LaunchAgent.
`--activate` starts it. New code needs a new endpoint/port while existing work
drains; do not overwrite an endpoint serving in-flight invocations. The legacy
CLI fixture/replay paths remain for isolated diagnostics, not as a second
production acquisition owner.

## Operations and verification

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
python3 scripts/restate/recovery_e2e.py \
  --server /absolute/restate-server --worker /absolute/track-scrapper-v2 \
  --evidence /absolute/new-private-evidence-directory
```

It requires `TEST_DATABASE_URL`, creates and removes its own schema and processes,
replays a local fixture twice, accepts work while the worker is stopped, restarts
the native server with the same state, replaces the worker and verifies retained
invocation IDs, hashes and a reopened single-sheet XLSX. Its evidence explicitly
separates those protocol checks from native chunk recovery and provider-outage
scenarios. It never sends a source request to a public provider.

Official references: [durable versioning](https://docs.restate.dev/services/versioning),
[configuration](https://docs.restate.dev/services/configuration),
[snapshots and backups](https://docs.restate.dev/server/snapshots),
[Rust SDK](https://docs.rs/restate-sdk/0.13.0/restate_sdk/),
[disabling protocol retries](https://docs.rs/reqwest/0.13.5/reqwest/retry/fn.never.html).
