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
- **Contact:** a public professional mailbox tied to its published school,
  person/office role, sport, source URL, and time/season when published.
- **Page:** a bounded acquisition/checkpoint entry, not a generic workflow job.
- `scrape`: configuration → acquire/replay → parse → transactionally save rows
  and the next page/cursor. HTTP and ordinary Chromium are source settings.
- `export`: configuration → consistent database snapshot → comparable bests →
  verified new XLSX. It performs no source access or database repair.

Configuration selects lower-48 states, graduation cohorts, source transports,
season, finite page limit, RPS, automatic-attempt limit, and output splitting.
The sample cohort is a user-editable setting, not an inferred graduation year.
Alaska, Hawaii, D.C., and territories are outside the requested national scope.

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
| `contacts` | School, person/office, role/sport, public email, source URL and observed/effective time; school-office and coach roles remain distinct |
| `scrape_pages` | Unique source/page key, URL/cursor/context, status, attempts, last success/error, and notes from the latest successful parse |

**C1 — atomic checkpoint:** a parsed page's entity upserts and next-page/cursor
advance commit together. Constraint/parse failures cannot advance success.

**C2 — replay:** source athlete IDs prevent duplicate people within a site.
Publisher result IDs update corrections. Missing IDs use tested source-specific
keys containing the publisher's page/event/round/row identity, never just a mark.
Old results are not replaced by a single athlete PR field.

**C3 — identity:** reliable explicit cross-profile evidence may link sites.
Conflicting school/identity evidence is retained and flagged. Name-only matching,
guessed graduation years, invented school IDs, and guessed mailboxes are forbidden.
Ambiguous cross-site identities stay separate and appear in Issues.

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

**C7 — spreadsheet:** selected known cohorts/states, one accepted local athlete
per row, school/postal and distinct coach/school email fields, observed PR columns,
and source links. Unknown/conflicting identities or missing facts are visible in
an Issues sheet. Optional athlete sheets use the same snapshot. Split oversized
outputs without exceeding Excel bounds. Strings are written as strings, not
interpreted formulas.
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
