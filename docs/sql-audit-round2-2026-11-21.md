# SQL correctness audit, second pass — November 21, 2026

## Scope

This pass continues from `be1f3ff179cfa7dfdfe728b30f013e032ebd44f8`, following the [first SQL audit](sql-audit-2026-11-21.md). It covers additional identity, contact, checkpoint and export-review defects. The Class of 2028 campaign remains Michigan, Minnesota, Iowa, Illinois and Colorado; graduation year is distinct from competition season. Shared persistence fixes apply to all cohorts. This branch does not include or reconstruct the earlier Class of 2028 preparation commit.

Database execution uses the same owned PostgreSQL 18.1 instance, ordinary application role and isolated test schemas. Source parsing uses retained local fixtures or explicitly constructed HTML. No live athlete database or provider requests are part of this audit.

## Confirmed defects

### Concurrent profile splits create a phantom canonical athlete

Two profiles sharing one canonical athlete can both decide to split before either commits. Each moves its own profile and results into a new canonical row. The old row then has no profiles or results but retains a name, making it eligible for export.

The controlled failing schedule produced three named athletes, two profiles, two results, one named orphan and three export rows. The expected state is two athletes with one profile and result each. Durable receipts had committed successfully, so this was retained database inconsistency rather than a transient failed attempt.

The fix adds a separate guarded deletion after the existing update of the previous canonical row. It deletes only a canonical with no source profiles and no results. A fresh statement after the existing canonical-row serialization sees the competing committed move. No new profile lock order is added. Reviewed identity evidence remains in profile JSON, and reviewed-result links keep their profile reference and immutable audit snapshots.

The tests cover the failing split, exact reviewed-audit retention, correct result ownership, exports and receipt replay after reconnect. A separate same-as control already passed before the fix and retains its bounded handling of existing PostgreSQL deadlock/lock-timeout errors. This patch does not claim to eliminate arbitrary identity workflow deadlocks.

### Concurrent contact revisions retain obsolete current claims

Two original contact URLs can redirect to the same final staff page. A normal coach-only parse contains no school update. When a prior capture is uncommitted, a later capture's old-claim UPDATE sees no prior rows, so both sets of claims can remain current. An empty later capture can likewise miss the prior claims entirely.

The failing real-parser fixtures retained six current coaching mailboxes instead of three, and retained three current mailboxes after a later empty capture. A separate initially absent school/office scenario retained the previous office mailbox after an empty revision, instead of withdrawing it. This covers the ordering before office-claim review.

The fix takes one schema-scoped transaction advisory lock on `(source, school_id)` before office review, school persistence and coach updates. The acquisition is a separate SQL statement, so later queries receive fresh snapshots after waiting. It also works before a school row exists. Non-contact pages add no query. Superseded evidence remains stored with review notes; an unconfirmed school-office mailbox is withdrawn.

### Duplicate checkpoint enqueue waits on rows it cannot change

Non-refresh batches used `ON CONFLICT DO UPDATE ... WHERE FALSE`. Despite updating zero rows, this locks an existing checkpoint owned by a durable worker. The same behavior affected non-contact items routed through the scalar mixed-contact path.

Both initial regressions observed the unnecessary dependency through `pg_blocking_pids`, then released the owner and drained the enqueue. A further real-parser durable scenario produced PostgreSQL's `deadlock detected` error while two page operations owned different checkpoints and contended on shared profile facts and parsed navigation.

The fix uses `DO NOTHING` for insert-only batches and routes non-contact, non-refresh scalar entries through that path. Explicit refresh and contact alias suppression remain separate. Tests preserve exact existing checkpoint JSON, insertion counts, sequence consumption, new pending status and attempts. PostgreSQL can still wait on a genuinely competing uncommitted insertion of the same unique key.

### Equivalent state filters omit Review rows

Review SQL compared an uppercased stored state directly with the caller's unnormalized array, while athlete and issue queries normalize case and Unicode whitespace. The public snapshot/export API returned two Wisconsin Review rows for `WI` and none for `wi`.

The fix uses symmetric Unicode trimming and lowercase comparison. Tests compare exact Review cells for uppercase, lowercase and Unicode-padded input, and cover stored padded acquisition state taking precedence over a different context state. Normal validated configuration uses canonical state codes; the reproduced bug is in public snapshot/export API consistency.

### Invalid reviewed links disappear from their acquisition scope

The generic unassigned phase excludes results having a reviewed-link audit record. The invalid-link phase originally scoped only against the target's current school or canonical state. A supported current-affiliation transfer could therefore invalidate the link and remove the still-unowned source result from its original state's issue list.

The failing reviewed-manifest fixture retained an unchanged unowned Wisconsin result, moved its reviewed target to Minnesota, and recorded `target_snapshot_changed`. Minnesota showed the issue; Wisconsin showed none.

The fix also includes represented-school and same-provider acquisition-page scope, matching existing unassigned-result rules. It keeps the precise invalid-link issue instead of adding a duplicate generic unassigned issue. Tests cover each relevant state, combined-state deduplication, exclusion of an unrelated state and unchanged source ownership/evidence.

### Masked PR rows suppress the missing-public-results diagnostic

The source-availability query treated the presence of any retained PR row as evidence of a public mark. MileSplit parsing retains masked PR metadata without publishing a performance, so a masked-only profile with restricted progression incorrectly lost its Review diagnostic.

The real-parser regression produced no Review rows where the masked-only profile required one; an unmasked headline is a control. The fix excludes only JSON boolean `masked: true` from public PR evidence. Controls preserve existing behavior for absent, null, false, string, array and object metadata. No hidden mark is inferred or recovered.

## Qualification

This pass adds 14 regression tests. Controlled failing-before runs reproduced all six defects; the final focused run passed 65 scenarios in 12 suites. The focused scenarios are included in the full integration total and must not be added to it.

| Check | Observed result |
| --- | --- |
| Focused SQL and adjacent regressions | 12 suites, 65 passed, zero failures or ignored tests |
| Every Cargo integration target | 124 suites, 594 passed, zero failed, ignored or filtered tests |
| Canonical library tests | 412 passed, 46 failed, two pre-existing performance tests ignored |
| Other five Moon lanes | Formatting, check, production Clippy, release build and documentation passed |
| JSON mask semantics | All nine PostgreSQL truth-table cases matched the intended public-evidence rule |
| Native Restate recovery | Passed with newly built binaries, durable replay and a reopened export workbook; zero provider requests |

The canonical `moon ci --force` exit status is **1**, not a successful full gate. All 46 failures were browser tests reporting Chromium exit before its debugging websocket URL was available. Forty-five explicitly reported `socket() failed: Operation not permitted`. The remaining startup exit retained only a Chromium shell-wrapper error; its cause is not established by the log. No failure was reported outside `browser::`. No check was disabled or weakened. Browser behavior still requires successful qualification in a suitable environment before rollout.

The native proof retained 2 athletes, 2 source profiles, 81 results and 2 page receipts through its exercised recovery and replay path. It verified a restored Restate server-state backup and reopened the workbook, including the published graduation-year and Review-cell assertions. Its owned schema and processes were cleaned up. This is fixture-based recovery evidence; it does not prove PostgreSQL backup restoration, a complete five-state campaign, provider-outage recovery, mid-chunk process death or an old-worker version upgrade.

The downloadable delivery separates this pass's logs and executable identities from first-audit evidence. It includes the original failing traces, exact integration target list and command, JSON mask truth table, native reports and apply/verification instructions. These changes have not been published to a remote branch or applied to a live athlete database. This pass measures no production throughput improvement.

The [PostgreSQL performance contract](postgresql-performance.md) records the lasting invariants and targeted reproduction commands. No schema migration, index addition, planner override or global memory/durability setting is introduced.

## References

- [PostgreSQL INSERT conflict conditions and row locking](https://www.postgresql.org/docs/18/sql-insert.html#SQL-ON-CONFLICT).
- [Read Committed statement visibility](https://www.postgresql.org/docs/18/transaction-iso.html#XACT-READ-COMMITTED).
- [Transaction advisory lock functions](https://www.postgresql.org/docs/18/functions-admin.html#FUNCTIONS-ADVISORY-LOCKS).
- [JSONPath comparison, filter and lax-mode semantics](https://www.postgresql.org/docs/18/functions-json.html).
