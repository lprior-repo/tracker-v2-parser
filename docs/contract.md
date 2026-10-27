# Scraper rewrite contract

This contract implements the owner's six-table, two-command design. It replaces
Part 1's SQLite bootstrap. The reuse source is
`/home/lewis/src/ad-law-scrape/athletic-rust-pipeline` (main `280492f`); its parsers,
fixtures, event rules, and source limits are useful, not its Fjall/Restate runtime.

## Domain and commands

- **School:** source-backed school identity with a persistent database UUID,
  postal fields, official website, and public school-office mailbox. A name is
  not an identifier. An NCES row is not automatically an athletics-site school.
- **Athlete:** local identity with source mappings, observed school, competition
  category, optional published graduation year, and `needs_review`.
- **Result:** one published result, including failed/no-mark rows and relay team
  results. An unknown athlete reference remains unassigned, never a fake person.
- **Contact:** a published professional contact claim tied to its source school,
  person/office role, sport, source URL, and time/season when published. A named
  track/XC coach may have no published mailbox; a uniquely associated mailbox
  may have no published person name. Neither fact is guessed.
- **Page:** a bounded acquisition/checkpoint entry, not a generic workflow job.
- `scrape`: configuration → acquire/replay → parse → transactionally save rows
  and the next page/cursor. HTTP and ordinary Chromium are source settings.
- `export`: configuration → consistent database snapshot → comparable bests →
  verified new XLSX. It performs no source access or database repair.

Configuration selects lower-48 states, graduation cohorts, source transports,
season, finite page limit, RPS, automatic-attempt limit, and output splitting.
The sample cohort is a user-editable setting, not an inferred graduation year.
Alaska, Hawaii, D.C., and territories are outside the requested national scope.
The initial checked-in acquisition/export scope is Wisconsin, with MileSplit
enabled and Athletic.net disabled until ordinary public access is available.
Expanding the configured state list is explicit, not an automatic national crawl.


## Storage contract

Exactly six application tables; ordinary SQLx migration metadata is not an
application entity. There are no capture, receipt, decision, export-generation,
event/state/year, distributed-lease, or model-service tables.

| Table | Identity and retained facts |
|---|---|
| `schools` | Unique persistent UUID and stable source school key; name/state/city/address/ZIP/website/public school email, source URL, field provenance and last checked time |
| `athletes` | Local identity; name, published graduation year, school, category, review flag |
| `athlete_sources` | Unique `(source, source athlete ID)` → local athlete ID and profile URL |
| `results` | Stable source result key; athlete when known, event/sport/meet/date, raw mark, exact normalized integer/unit, timing/wind/specification/status, represented school, source URL, last seen and small raw JSON |
| `contacts` | Source contact claim: school, person/office, exact published role/sport, nullable public email, source URL, observed/effective time, raw typed claim and SHA-256 source key; office and coach kinds remain distinct |
| `scrape_pages` | Unique source/page key, URL/cursor/context, status, attempts, last success/error, and notes from the latest successful parse |

**C1 — atomic checkpoint:** a parsed page's entity upserts and next-page/cursor
advance commit together. Constraint/parse failures cannot advance success.

**C2 — replay:** source athlete IDs prevent duplicate people within a site.
Publisher result IDs update corrections. Missing IDs use tested source-specific
keys containing the publisher's page/event/round/row identity, never just a mark.
Old results are not replaced by a single athlete PR field.
Contacts use `UNIQUE(school_id, kind, source_key)`, not canonical-person identity.
The 64-hex `source_key` is SHA-256 of PostgreSQL
`jsonb_build_array(source_url,name,role,sport,lower(btrim(email)))::text` encoded
as UTF-8, with SQL null preserved. Backfill and coach/office inserts use the same
expression. Unchanged claims replay without duplicates; changed source URL,
name, role, sport or normalized mailbox can remain separate claims. Different
named coaches sharing a mailbox, different sports and separate no-email coaches
must not collapse. The new migration preserves existing rows and prior migration
checksums. Nonnull email still must be valid; every contact must have a nonblank
published name or mailbox. Capture/check time is not evidence of current tenure.
Contacts-page persistence first flags earlier claims for the exact source school
and original/effective page URL, then upserts the current parsed claims in the
same transaction. Changed or removed claims remain retained with an explicit
earlier-unconfirmed review annotation; unchanged claims clear that annotation
without changing their IDs. An incomplete empty parse cannot prove absence.
Failed transactions roll back annotations, entities, and checkpoint together.
Completed contact captures may be reprocessed without another automatic attempt,
but a blocked contact authority still prevents that replay.


**C3 — identity:** reliable explicit cross-profile evidence may link sites.
Conflicting school/identity evidence is retained and flagged. Name-only matching,
guessed graduation years, invented school IDs, and guessed mailboxes are forbidden.
Ambiguous cross-site identities stay separate and appear in Issues.
Contact names are never inferred from email spelling. Program headings cannot
lend track/XC scope to unrelated basketball/football cards; published role text
and sport scope remain source claims rather than rewritten identities.
District-wide or unmatched school attribution and unpublished current tenure
remain explicit per-contact review notes. They set the athlete's `needs_review`
and appear contextually in `review_notes` and `Issues`, not as accepted school or
current-contact facts. Explicit non-track coaching roles cannot inherit track
responsibilities from a contradictory program label. Hidden and page-chrome
text cannot supply a coaching role or mailbox.


**C4 — source boundaries:** parsed source DTOs may contain unknown optional
facts. Database writes enforce nonempty stable keys, valid references, and valid
state/status domains. Unknown result identities/relay teams use a nullable athlete
reference plus reported members/raw source values, not a canonical placeholder.

The school-address import accepts published NCES CSV artifacts from
`lprior-repo/rust-athletic-school-addresses`. It verifies the supplied SHA-256,
validates source IDs and rejects duplicate IDs before applying one transaction.
NCES IDs remain strings so leading zeros survive. Public-school datasets include
multiple grade levels; importing them does not classify every row as a high
school, establish athletics participation, or supply missing private-school data.
Coordinates and reported validation values remain source claims in provenance.
An unspecified address kind is never relabeled as verified physical or mailing.

Imported NCES schools have their own source identities. Existing athletics-site
schools are enriched only with unambiguous name/state/city candidates corroborated
by an existing matching address or a website unique to both directories. Conflicts
remain unchanged. UUID assignment preserves existing numeric foreign keys and
does not merge source rows. Replaying an import cannot regenerate a UUID.


## Page transitions and failures

`pending → attempt → succeeded` only follows a successful parse and transaction.
A transient failure remains unfinished and may be attempted again within the
configured ceiling of three total automatic attempts. Authentication, challenges,
paywalls, and rate-limit refusals stop the source rather than rotating transports,
proxies, identities, or solving a CAPTCHA. Interrupted unfinished pages resume
without discarding previously committed data. Explicit refresh is distinguishable
from automatic retry.
Official-school contact access stops are scoped to the affected authority across
providers; they do not stop unrelated contact sites or athlete acquisition.
Embedded public contact-form widgets/sign-in links are not full-page challenges.
Resumed acquisition selects only the configured states, including saved work
from earlier runs. State-less Athletic.net directory bootstraps require an exact
selected-state filter; unknown scope does not authorize unrelated national work.
Meet continuations preserve their discovery scope without declaring a venue's
state. Out-of-scope checkpoints and consumed attempts stay unchanged, and their
exhausted failures do not fail the selected-state run. Provider-wide access
stops still apply regardless of selected state.

The Issues export selects relevant acquisition pages and unassigned results
using state, source school identity, and source-page provenance. A Wisconsin
file must not include unrelated national page backlogs. An empty internal
report scope explicitly requests all states; uncertain athlete visibility is
unchanged.



One running scraper owns the acquisition session. Concurrency is bounded at one
initially. Source requests are paced at no more than 1 RPS; supplemental official
school pages use conservative pacing. No distributed leases or fencing protocol.
Response bodies, redirects, parser/frontier expansion, attempts, and each command's
page budget have finite bounds. Source failures never mean "no athletes found."

Error classes remain observable: configuration, transport/timeout, access stop,
representation/schema, database/transaction, checkpoint, and export/publication.
No ignored fallible results or source failures converted to successful empty data.

## Pure core and actions

Data: ordinary source records and validated configuration/IDs. Calculations:
source parsing, exact mark conversion, event/comparability keys, and best selection.
Actions: HTTP/browser access, environment/configuration reads, PostgreSQL, and XLSX
I/O. No routine scraping requires a language model. Local GPU coding/review
assistance is a development activity, never the production data authority.

First-party Rust forbids unsafe code. Production unwrap/expect/panic/todo,
unbounded resource growth, unchecked arithmetic/casts/indexing, and swallowed
errors are excluded. Strict Clippy applies to production, not test setup style.

## Comparable results and output

**C5 — exact marks:** preserve the original mark. Normalize time to integer
nanoseconds, distance to integer micrometres, and points to integer centipoints
using checked exact conversions. Time minimizes; distance/points maximize.
DNS/DNF/DQ/no-mark/malformed/nonfinite results remain stored but never win a PR.

**C6 — compatibility:** separate indoor/outdoor/XC, distinct race distances,
hand/FAT/unknown timing, wind classes, implement/hurdle specifications, and XC
course context. No unsupported distance conversions, timing adjustment, or
estimated relay splits. Unknown contexts are not silently treated as equivalent.
Bests mean best comparable **observed** results, not certified complete careers.

**C7 — spreadsheet:** one local athlete ID per row, school/postal fields,
`coach_names`, distinct coach/school email fields, observed PR columns, and source
links. Coach email aggregation omits null; a named coach without email remains
visible in `coach_names` with a blank email when no mailbox is published.
Missing coach names and mailboxes are explicit review issues and set
`needs_review`, including partial missing facts when another coach has them. Known
out-of-scope cohorts/states remain excluded. Unknown cohorts/states remain blank
and visible as review candidates, not guessed into accepted membership.
Review-flagged athletes remain on their single row with explicit `needs_review`
and `review_notes`; conflicting identities and missing facts also remain visible
in `Issues`. Non-comparable observations retain the original mark, reported
status and comparison rejection reason. Optional athlete sheets use the same
snapshot. Split oversized outputs without exceeding Excel bounds. Strings are
written as strings, not interpreted formulas. Full public profile metadata is
retained in ordered JSON chunks on `Profile_details`, not additional athlete PR
rows; URL-looking chunks stay literal text, not inferred hyperlinks.
Page notes, unfinished page status and errors, and unassigned results appear in
the Issues sheet with their source and URL, regardless of athlete filters. Each
split workbook includes these snapshot-wide issues; a successful reparse replaces
that page's prior notes.


**C8 — publication:** rust_xlsxwriter writes a new sibling temporary file;
calamine reopens it and checks required sheets and expected row counts before
non-overwriting publication. Failure leaves no completed-looking partial file.

## Hazards and evidence boundary

| Hazard | Required behavioral evidence |
|---|---|
| Rollback advances a page | Invalid write leaves rows and checkpoint unchanged |
| Replay duplicates or conflates equal marks | Repeat/correction tests and distinct source row keys |
| Restart retries forever | Persisted attempt ceiling, interrupted-page replay |
| Namesake or conflicting identity merge | Separate source IDs; explicit-link and review-flag tests |
| Wrong PR from incompatible contexts | Exact conversion, direction, timing/wind/specification/XC tests |
| Contact misassociation | Unrelated/district/teacher mailbox exclusions and school-scoped postal assertions |
| False national completeness | Pending/failed/blocked pages and unresolved/missing facts remain visible |
| Partial or overwritten workbook | Reopen, row-count, split, and no-overwrite tests |
| Secret disclosure / private network fetch | No persisted tokens; source ownership and address-boundary checks |

Rust Contract modelling identifies these claims; it is not a formal proof.
Black Hat review must examine the actual implementation against C1–C8. Red Queen
challengers execute deterministic production-path scenarios and preserve any
found defect as a regression. Command output, not a model's assertion, decides
whether an executable check passes. Hosted release publishing and a completed
national data collection must not be claimed from local fixture tests.
