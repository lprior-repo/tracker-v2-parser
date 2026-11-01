# Native PostgreSQL performance contract

The workload runs on a shared 16 GiB Mac with an 8 GiB scraper stack target. Changes must preserve exact result identities, retained source evidence, manual research, access stops, the shared three-acquisition budget, page/domain/receipt atomicity and durable workflow recovery. A configured 3 RPS ceiling is an admission limit, not a throughput guarantee.

## Query and transaction boundaries

Two additive MileSplit HASH indexes support exact source-result ID and source-URL reconciliation. Original source, equality, JSON kind/mask, order and limit predicates remain authoritative; PostgreSQL rechecks hash collisions. HASH avoids a B-tree tuple-size failure on untrusted long identifiers. No uniqueness or historical result-key assumption is introduced. A raw meet row is inspected only if the exact lookup reaches it; unlinked observations are retained.

Result reference caches are transaction-local, positive only, and bounded to 128 performances and 1 MiB per chunk. Missing references retain the original lookup/error behavior. No cache crosses a page or an identity merge/split. Domain upserts remain ordered and atomic. Queue batching preserves duplicate precedence, sequence consumption, access stops, contact discovery and refresh behavior. Review writes use exact change guards. School ordering addresses the tested reversed-school writer pattern; it does not establish deadlock freedom for arbitrary research/identity transactions.

Immutable capture/parse digests and reference serialization are calculated before obtaining the page transaction locks. Their errors are consumed at the original validation points, after retained receipt replay checks. Domain writes, checkpoint success, receipt and attempt/operation completion remain one transaction. A single boxed action boundary bounds compiler type complexity without spawning detached work.

## Sampled persistence evidence

Every sixteenth positive checkpoint ID emits fixed-label timing records. Domain stages distinguish replay preparation, contact review, school writes, school conflicts, profiles, results, unassigned results, contacts, revision review, queue insertion and checkpoint update. Durable stages distinguish pool acquisition plus BEGIN, reservation locking, receipt lookup, domain persistence, receipt writes and final COMMIT. Timings use a monotonic clock and include failed stages. They contain no SQL text, parameter values, credentials, capture bodies or full source URLs. PoolAndBegin is not presented as pure pool wait; the journal commit stage is not presented as fsync alone. Counters and retained journal samples remain the independent throughput evidence.

## Retention and cold evidence

The November 1 inventory counted 1,908,482 observations: 1,626,333 raw meet rows lacked a linked athlete profile and 282,149 came from profile URLs. Result keys identify source observations independently of campaign generation. Unknown identity or graduation year is not evidence that an observation is obsolete.

There is no approved age-based deletion rule. Cold evidence remains recoverable in the database and immutable artifacts. Exact indexed fact lookups and narrow reference projections form the hot path. Do not rewrite the approximately 4 GiB athlete-source TOAST payload merely to reduce the relation-size display. Any future physical archive must inventory result/profile/school links, reviewed identities, export references, durable captures/parses, retry lineage and later cohorts; prove restored equality before removing a live reference. This upgrade performs no retention deletion or wholesale JSON backfill.

## Resource policy

Retain the SQLx pool cap of eight, PostgreSQL connection cap of forty and global work_mem of 4 MiB. The coverage selector uses a short read-only transaction with local work_mem of 16 MiB. Its restored-snapshot alternating trial reduced median latency from 969 to 845 ms and eliminated temporary-file I/O; real PostgreSQL tests verify restoration of the pooled backend's 4 MiB default after success, failure and cancellation. The optimized selector preserved all 10,292 ordered candidates and reduced the preceding baseline median from 2,076 to 977 ms before the local-memory change.

The 512 MiB shared-buffer trial removed buffer reads without a convincing latency gain under whole-host compression churn, so the 128 MiB baseline remains. A larger candidate requires separate measurement. Whole-host compression/swap deltas, available disk and runtime latency decide acceptance; summed backend RSS is not unique physical memory. Selection batching saved only two startup queries per 2,000-page rolling batch and could change greedy exclusions, so it was withheld; no frontier cache or planner override is installed.

Prepare configuration from fresh read-only PostgreSQL 18 observations, exact prior-file SHA256 and private owned files. Preserve existing preload entries, add pg_stat_statements only during a backed-up controlled restart, and verify effective settings and pending_restart afterward. Query metrics stay private. Maintain durability settings on. Per-table autovacuum/analyze changes require relation-specific churn evidence; avoid blanket VACUUM FULL, REINDEX, statistics resets or planner forcing.

## Capacity and recovery

Build artifacts may live on the dedicated external build cache. Private recovery data must use encrypted storage with ownership enabled. The encrypted recovery container and its private local key are separate; the live PostgreSQL data directory, WAL and Restate journals remain in place. Validate both backup-destination and actual live-data filesystem headroom before operations. Native backup preflight budgets twice the database, twice retained evidence/artifacts/exports, and a 2 GiB reserve; do not assume compression savings.

Every rollout requires all six Moon lanes, exact executable hashes, an isolated PostgreSQL restore, native Restate restart/replay/export and reopened workbook equality. Scheduler/admission command changes additionally require an old-worker partial journal replay through the same invocation and a versioned future continuation. Existing input bytes and recorded journal prefixes must remain unchanged. Keep the prior worker, configuration and recovery backup until post-handoff HTTP completions, frozen-input/prefix equality, five-ledger inventory equality and publication hashes are verified.

## Live acceptance

Use the existing ten-minute heartbeat for bounded 1/5/10-minute wire starts, at most 30 completed page-stage samples and 120 recent selector pairs, plus short read-only PostgreSQL wait/counter deltas with reset timestamps. Record phase mix and export gaps. Compare paired evidence after each accepted slice; database fixture speedups are not live RPS claims. Stop extra diagnostics when capacity or runtime latency is unsafe. State completion still requires the exact retained terminal report, verified receipt/publication/delivered workbook and a quiescent predecessor before submitting the next state once.
