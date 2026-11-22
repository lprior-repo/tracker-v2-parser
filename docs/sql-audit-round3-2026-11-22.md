# SQL correctness audit, PostgreSQL 18.7

## Scope and release target

This pass continues from `3ca35b2c7e25f5e9e0b35768410ea20e9ce40ace`, following the [second SQL audit](sql-audit-round2-2026-11-21.md). It fixes three additional defects in acquisition-scope queries, public-mark availability review, and reviewed athlete identity. The campaign target remains graduating Class of 2028 in Michigan, Minnesota, Iowa, Illinois and Colorado, with the 2026 competition season configured separately. These shared persistence/export corrections apply across cohorts. The earlier campaign-preparation commit is separate and is not reconstructed by this branch.

The owned audit server runs PostgreSQL 18.7. SQL reports `server_version = 18.7` and `server_version_num = 180007`; the installed client and server executables agree. The official [version policy](https://www.postgresql.org/support/versioning/) lists 18.7 as the current minor release of major 18 on November 22, 2026. PostgreSQL 19.0 is the latest major release, released November 19. This work retains the requested major 18 deployment target.

The source archive came from PostgreSQL's official distribution service and matched its published SHA-256, `19c2dc7d8520e10532d082b43c36c4d2dcc6cd9e40506c6f598e7338f84bbde7`. `pg_stat_statements` 1.12 was compiled from the same source tree. The server uses 128 MiB shared buffers, 4 MiB work memory and forty connections. The application retains its eight-connection SQLx pool. `fsync`, `full_page_writes` and `synchronous_commit` remain enabled. Database scenarios use an ordinary application role and isolated schemas; migration tests use a separate disposable-database administrator.

The earlier audits retain their actual PostgreSQL 18.1 evidence. This fresh-cluster qualification does not establish that an existing deployment has been upgraded. Review the [18.7 migration notes](https://www.postgresql.org/docs/18/release-18-7.html), including the referenced 18.6 notes when applicable, before any existing-cluster upgrade. No live athlete database was inspected or modified and no public-provider scrape was started.

## Confirmed defects and corrections

### Redirected result captures lose their acquisition-state issues

Checkpoints retain the requested target URL, while result persistence records the final capture URL. The issue query joined those fields as though they represented the same URL. A real retained MileSplit raw-results fixture contained eighty unowned observations with no represented school. An OH request redirected to an approved `www` URL; the unscoped issue export contained all eighty observations, while the OH export contained none. The direct-capture control passed.

Both unassigned-result and invalid-reviewed-link phases now retain the existing direct URL and school checks and also accept two precise associations. A MileSplit raw-result observation can match its retained parser-request URL. A committed durable receipt can match its captured response URL through that receipt's own selected-state checkpoint, with the source matching the result's source. `EXISTS` prevents duplicate issues from repeated captures and repeated selected states.

All committed receipt history remains eligible. The durable regression captures OH at one final URL, recaptures the same OH checkpoint in a later generation at another URL, then reconfirms through a WI alias at the original final URL. Only the older OH receipt preserves the relevant association. The regression also verifies exact issue equality, an unrelated successful MI checkpoint, duplicate suppression, unchanged observations and durable ledgers, zero wire requests, and receipt replay.

A separate public reviewed-link import and recapture scenario verifies phase 3: a changed requested URL invalidates the reviewed result snapshot, and the precise invalidation issue remains visible once in OH. Export preserves the source observation and audit records.

This correction preserves associations supported by retained request/response URLs. It does not reconstruct permanent per-result acquisition lineage after a later alias overwrites both current URLs with values never captured in an earlier state. That stronger guarantee requires an explicit stable file/result-to-checkpoint evidence relation and a separately qualified backfill of old retained artifacts. This patch adds no approximate URL parser, unbounded URL-history array, migration, or evidence rewrite.

### Placeholder PR metadata hides missing-public-mark diagnostics

The Review query previously treated any unmasked retained PR metadata row as proof of a public mark. The real parser retains empty, dash-only, em-dash and star-only rows as source evidence while emitting no performance. Eight such profile variants, across headline and sport-specific rows, therefore produced no Review entries despite explicitly restricted progression and no saved results. Two real-mark profiles provided controls.

SQL now returns bounded Review candidates and an internal athlete ID when retained metadata needs inspection. Rust examines that athlete through the existing bounded workbook projection and the same public-PR preparation rules used by the workbook. Placeholder metadata cannot suppress the diagnostic; real public marks still do. This works with previously retained evidence, including missing or null mask metadata, without modifying source JSON or adding SQL HTML matching rules.

The public Review document and frozen export formats remain unchanged. Inspection retains the component row/byte limits and the original eight-MiB athlete-record limit, loads one athlete at a time, and awaits bounded PR parsing outside the async executor. A failure or cancellation invalidates the snapshot before it can be reused.

The internal keyset advances past every examined candidate, including candidates rejected because they have public marks. A fully rejected batch therefore cannot prematurely end the export. Regressions cover one-row batches with public marks before, between and after diagnostics, mixed acquisition failures, several batch sizes, exact reopened workbook cells, immutable source evidence, and original-size rejection before PR parsing.

The correction bounds each batch and record, not total query work. An empty Review response may require inspecting every selected availability candidate. The additional legacy-evidence inspection is a correctness cost; this pass makes no speedup claim for that path.

### Locale-dependent SQL normalization splits reviewed Unicode identities

Reviewed-identity manifests normalized approved names with Rust's `trim().to_lowercase()`. Subsequent SQL checks used PostgreSQL `lower(btrim(...))`. On this PostgreSQL 18.7 libc `C.UTF-8` database, `İpek Çelik` becomes `ipek çelik` in PostgreSQL, while Rust produces `i\u{0307}pek çelik`. An approved pair imported successfully, then an unchanged real-parser replay split one canonical person into two. The unapproved-name split control passed before the correction.

The importer and subsequent identity checks now share the same Rust normalizer. Group validation fetches at most three narrow rows and accepts only an exact two-profile group with valid owner, membership, review audit, common manifest digest and approved names. The third-row bound preserves refusal to absorb an unreviewed additional profile. The query does not transfer captured pages, raw HTML or the full audit. Valid accepted-name arrays remain bounded by the manifest import limits.

The change adds no SQL round trips or locks and requires no migration. Regression scenarios preserve one approved person, twelve correctly owned results and the exact review audit across repeated unchanged captures. An unapproved name still causes the legitimate split. Replaying the durable approval receipt leaves both resulting states unchanged and cannot silently remerge a later split.

## Query-plan qualification

The actual corrected issue query was measured with `EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)` on PostgreSQL 18.7, using 100,000 synthetic observations, 50,000 checkpoints and 50,000 committed receipts with 4 MiB work memory. Four phase-2 unassigned-result selections cover a selected state with fewer matches, one with more matches, a state with no matches, and a later result-ID keyset page. Every statement had a thirty-second deadline. These plans do not constitute separate plan qualification for phase 3; its correctness is covered by the public reviewed-link regression.

The unindexed plans completed and inspected the receipt relation once per query, without a receipt scan repeated for each result. The three matching cases returned the requested 1,000 issues; the unrelated-state case returned zero. No temporary blocks were recorded. A disposable expression hash index on the captured URL was then compared; PostgreSQL did not use it and retained the same receipt scan shape. The index was removed from the disposable fixture and no production migration was added.

The evidence package retains the query, synthetic setup, both sets of JSON plans, settings and execution statuses. Compilation ran concurrently, so elapsed timings are observational and are not production throughput evidence. Output row limits do not guarantee a fixed bound on all rows examined, and these fixture plans do not establish the plan for every production cardinality or parameter distribution.

## Verification commands

All commands use the checked-in Rust toolchain, Moon 2.2.4, the owned PostgreSQL 18.7 instance and retained local fixtures. Test database roles and URLs must refer to disposable resources.

```sh
cargo test --locked --test export_redirected_result_scope -- --nocapture
cargo test --locked --test export_review_placeholder_profile --test reviewed_identity_unicode --no-fail-fast -- --nocapture
cargo test --locked --test reviewed_identity_unicode --test athlete_identity_review --test athlete_identity_boundaries --no-fail-fast -- --nocapture
cargo test --locked --test export_redirected_result_scope --test reviewed_result_links --test export_acquisition_review --no-fail-fast -- --nocapture
cargo test --locked --test export_review_placeholder_profile --test export_review_masked_profile --test review_query_bounds --test review_source_unavailable --test export_snapshot_review --test reviewed_result_links --no-fail-fast -- --nocapture
env -u CI moon run pipeline:fmt-write
moon ci --force
cargo test --workspace --all-features --locked --test '*' --no-fail-fast
cargo test --workspace --all-features --locked --test export_redirected_result_scope --test export_review_placeholder_profile --test reviewed_identity_unicode --test reviewed_result_links --no-fail-fast
cargo test --workspace --all-features --locked --lib transport::http::rate_tests -- --test-threads=1 --nocapture
env -u CI -u CI_NAME -u AZURE_PIPELINES moon run pipeline:native-recovery -- --server /absolute/restate-server --worker /absolute/track-scrapper-v2 --evidence /absolute/new-evidence
```

Targeted pre-fix runs reproduced all three defects. Nine new regression scenarios cover the corrected behaviors. Expanded focused runs passed 64 distinct scenarios across eleven suites; those scenarios are included in the full integration total, not additional tests.

## Final qualification

| Check | Observed result |
| --- | --- |
| Running PostgreSQL server | 18.7; `server_version_num = 180007` |
| All integration targets | 127 suites; 603 passed, zero failed or ignored; exit 0 |
| Final focused run after lint cleanup | Four suites; 20 passed, zero failed or ignored; exit 0 |
| Release build | Workspace, all features, locked dependencies; exit 0 |
| Canonical `moon ci --force` | Five lanes completed, one failed; exit 1 |
| Final library test lane | 411 passed, 47 failed, two ignored |
| Isolated HTTP pacing rerun | Both rate scenarios passed; exit 0 |
| Native Restate recovery | Actual release executables, isolated PostgreSQL schema; passed with cleanup |

The successful canonical lanes were formatting, all-target checking, strict production Clippy, release building and documentation. The initial lint run found two redundant `pub(crate)` declarations inside already restricted modules. Removing the redundant restriction preserved effective visibility; strict lint then passed. The full integration run preceded these two visibility-only corrections. The final twenty-scenario run, canonical compilation and native recovery used the final production source.

All forty-six final browser failures ended before Chromium exposed its websocket endpoint and explicitly reported `socket() failed: Operation not permitted`. Those browser behaviors remain unqualified in this environment. The remaining failure was the unchanged two-RPS HTTP redirect test's receive-interval assertion. That test passed in the initial gate and both HTTP rate cases passed in a subsequent isolated run with no competing builds or test runs.

The HTTP assertion measures fixture receive timestamps; production pacing reserves a start before connection/request processing. Unequal scheduling or connection delay can shorten receive intervals despite correct reservation spacing. The failed run recorded neither the intervals nor reservation timestamps, so the precise cause is unproven. The isolated pass does not erase the canonical failure. No transport code, assertion, browser permission or quality gate was changed. A recurrence needs correlated reservation, request-execution and receive timestamps to distinguish test timing from an actual pacing defect.

### Native recovery evidence

The final recovery used Restate server 1.8.1 and the release worker against PostgreSQL 18.7. The native Moon executable was invoked directly after the canonical gate completed. An earlier npm-launcher invocation had returned zero with an empty log and no proof directory; that attempt was explicitly rejected as evidence. The final invocation produced the actual report and exited zero.

The report verifies two athletes, two source profiles, eighty-one results and two page receipts after duplicate offline submissions, with zero provider wire requests. Recovery stopped the worker during export acceptance, gracefully stopped the server, copied and verified twenty-eight Restate state files, started from the verified clone, and completed the same invocation. Duplicate delivery remained idempotent. The immutable workbook reopened with the published Class of 2028 athlete, the expected PR cells and Review entries, while excluding the fixture with unknown graduation year. Intentional edits to delivery copies did not alter the immutable workbook or canonical pointer.

The proof also exercised bounded-stage capacity exhaustion and retry, unchanged completed journal steps, unchanged database facts, CLI workflow continuation, and restored workflow reports. Cleanup confirms graceful server/worker shutdown and removal of the isolated schema. The report records exact SHA-256 digests of the server, worker and native operator executable.

This is a Restate state recovery proof, not an in-place PostgreSQL upgrade or an old-worker rolling upgrade. A separate disposable PostgreSQL migration/restore scenario passed within the integration suite. No previous worker was supplied to the native proof. No public-source outage or live five-state campaign was exercised.

## Handoff limits

This audit is committed on `audit/sql-correctness-2028` and delivered as incremental and cumulative patches plus a cumulative Git bundle. The cumulative history starts after public base `51395463f3ad8a58c3f435775c5584a974f1d393`, includes the Linux prerequisite and both earlier SQL audits, and requires that base history when importing the bundle. Publication to GitHub and deployment to a live database are not part of this handoff.

The checked-in starting configuration still selects Wisconsin and graduation year 2027. It must not be mistaken for the separate Michigan/Minnesota/Iowa/Illinois/Colorado Class of 2028 campaign configuration. These fixes do not reconstruct that earlier campaign-preparation commit or certify campaign readiness. Full rollout qualification also remains blocked by the recorded canonical test failures. The delivery package retains the successful checks, failed runs, final rerun, query plans, native proof and exact application instructions so the remaining checks can be reproduced without losing this work.
