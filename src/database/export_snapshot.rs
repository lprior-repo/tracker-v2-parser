use std::{
    error::Error,
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, ensure};
use futures::{StreamExt, TryStreamExt, stream};
use sqlx::{PgPool, Postgres, Transaction};

use crate::{
    export::{ExportIssue, ExportOptions, StagedExport},
    model::AthleteRecord,
};

pub const EXPORT_BATCH_ROWS: usize = 1_000;
pub const EXPORT_BATCH_BYTES: usize = 8 * 1024 * 1024;
pub const EXPORT_MAX_OBSERVATIONS: i64 = 10_000;
pub const EXPORT_COMPONENT_BYTES: i64 = 32 * 1024 * 1024;
pub const EXPORT_COLLECTION_BYTES: usize = 32 * 1024 * 1024;
pub const EXPORT_COLLECTION_ROWS: usize = 10_000;

#[derive(Debug)]
pub struct ExportSnapshotInvalidated;

impl fmt::Display for ExportSnapshotInvalidated {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("export snapshot was invalidated by an interrupted or failed batch; start a new snapshot")
    }
}

impl Error for ExportSnapshotInvalidated {}

#[derive(Debug)]
pub struct ExportLimitExceeded {
    pub kind: &'static str,
    pub id: i64,
    pub actual: i64,
    pub limit: i64,
}

impl fmt::Display for ExportLimitExceeded {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "export {} {} requires {}, exceeding limit {}; no complete export was published",
            self.kind, self.id, self.actual, self.limit
        )
    }
}

impl Error for ExportLimitExceeded {}

pub struct ExportSnapshot<'a> {
    transaction: Transaction<'a, Postgres>,
    states: Vec<String>,
    years: Vec<i32>,
    athlete_cursor: i64,
    issue_cursor: (i32, i64, i64),
    records_finished: bool,
    issues_finished: bool,
    pending_record: Option<(AthleteRecord, usize)>,
    record_sql: Arc<str>,
    usable: bool,
}

type IssueRow = (
    Option<String>,
    Option<String>,
    Option<String>,
    i32,
    i64,
    i64,
    i64,
    bool,
);

fn decode_issue_batch(rows: Vec<IssueRow>) -> Result<(Vec<ExportIssue>, usize)> {
    rows.into_iter().try_fold(
        (Vec::new(), 0_usize),
        |(mut issues, bytes), (source, url, detail, _, id, _, size, _)| {
            let source = source.ok_or(ExportLimitExceeded {
                kind: "coverage issue serialized bytes",
                id,
                actual: size,
                limit: i64::try_from(EXPORT_BATCH_BYTES)?,
            })?;
            let bytes = bytes
                .checked_add(usize::try_from(size)?)
                .context("coverage batch byte counter overflow")?;
            issues.push(ExportIssue {
                source,
                url: url.context("missing bounded issue URL")?,
                detail: detail.context("missing bounded issue detail")?,
            });
            Ok((issues, bytes))
        },
    )
}

#[tracing::instrument(skip(pool))]
pub(super) async fn validate_schema(pool: &PgPool) -> Result<()> {
    let mut transaction = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *transaction)
        .await?;
    let applied: Vec<(i64, bool, Vec<u8>)> =
        sqlx::query_as("SELECT version, success, checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&mut *transaction)
            .await
            .context("reading existing schema migration metadata without changing it")?;
    let migrator = sqlx::migrate!("./migrations");
    let expected: Vec<_> = migrator
        .iter()
        .filter(|migration| !migration.migration_type.is_down_migration())
        .collect();
    ensure!(
        applied.len() == expected.len(),
        "schema migration history is missing, outdated or newer than this exporter"
    );
    ensure!(
        applied
            .iter()
            .zip(expected)
            .all(|((version, success, checksum), migration)| *success
                && *version == migration.version
                && checksum.as_slice() == migration.checksum.as_ref()),
        "schema migrations are incomplete or incompatible with this exporter"
    );
    validate_reports(&mut transaction).await?;
    transaction
        .commit()
        .await
        .context("finishing read-only schema validation")
}

async fn validate_reports(transaction: &mut Transaction<'_, Postgres>) -> Result<()> {
    let records = format!(
        "SELECT document FROM ({}) report(document) LIMIT 0",
        include_str!("athlete_records.sql")
    );
    sqlx::query(sqlx::AssertSqlSafe(records))
        .bind(Vec::<String>::new())
        .bind(Vec::<i32>::new())
        .execute(&mut **transaction)
        .await
        .context("validating existing athlete export schema without writes")?;
    let issues = format!(
        "SELECT source,url,detail,kind,item_id,ordinal,bytes,phase_exhausted FROM ({}) issues LIMIT 0",
        include_str!("export_issues.sql")
    );
    sqlx::query(sqlx::AssertSqlSafe(issues))
        .bind(Vec::<String>::new())
        .bind(-1_i32)
        .bind(0_i64)
        .bind(0_i64)
        .bind(i64::try_from(EXPORT_BATCH_BYTES)?)
        .bind(i64::try_from(EXPORT_BATCH_ROWS)?)
        .execute(&mut **transaction)
        .await
        .context("validating existing coverage export schema without writes")?;
    Ok(())
}

impl<'a> ExportSnapshot<'a> {
    #[tracing::instrument(skip(pool, states, years))]
    pub(super) async fn begin(pool: &'a PgPool, states: &[String], years: &[i32]) -> Result<Self> {
        let mut transaction = pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *transaction)
            .await?;
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM athletes WHERE false")
            .fetch_one(&mut *transaction)
            .await?;
        let records = include_str!("athlete_records.sql")
            .replacen(
                "WHERE p.school_id IS NOT NULL",
                "WHERE p.athlete_id = $3::bigint AND p.school_id IS NOT NULL",
                1,
            )
            .replacen(
                "WHERE a.name IS NOT NULL",
                "WHERE a.id = $3::bigint AND a.name IS NOT NULL",
                1,
            );
        let record_sql = format!(
            "SELECT CASE WHEN octet_length(document::text) <= $4::bigint THEN document::text END, octet_length(document::text)::bigint FROM ({records}) report(document)"
        );
        Ok(Self {
            transaction,
            states: states.to_vec(),
            years: years.to_vec(),
            athlete_cursor: 0,
            issue_cursor: (0, 0, 0),
            records_finished: false,
            issues_finished: false,
            pending_record: None,
            record_sql: record_sql.into(),
            usable: true,
        })
    }

    #[tracing::instrument(skip(self))]
    pub async fn next_records(&mut self, max_rows: usize) -> Result<Option<Vec<AthleteRecord>>> {
        ensure!(self.usable, ExportSnapshotInvalidated);
        self.usable = false;
        let result = self.read_records_batch(max_rows).await;
        if result.is_ok() {
            self.usable = true;
        }
        result
    }

    async fn read_records_batch(&mut self, max_rows: usize) -> Result<Option<Vec<AthleteRecord>>> {
        ensure!(
            (1..=EXPORT_BATCH_ROWS).contains(&max_rows),
            "export record batch must contain 1..={EXPORT_BATCH_ROWS} athletes"
        );
        if self.records_finished && self.pending_record.is_none() {
            return Ok(None);
        }
        let (_, records, _, _) = stream::iter(0..max_rows)
            .map(Ok::<_, anyhow::Error>)
            .try_fold(
                (self, Vec::new(), 0_usize, false),
                |(snapshot, mut records, bytes, full), _| async move {
                    if full {
                        return Ok((snapshot, records, bytes, full));
                    }
                    let Some((record, size)) = snapshot.read_record().await? else {
                        return Ok((snapshot, records, bytes, false));
                    };
                    let total = bytes
                        .checked_add(size)
                        .context("export record byte counter overflow")?;
                    if total > EXPORT_BATCH_BYTES {
                        snapshot.pending_record = Some((record, size));
                        return Ok((snapshot, records, bytes, true));
                    }
                    records.push(record);
                    Ok((snapshot, records, total, false))
                },
            )
            .await?;
        Ok((!records.is_empty()).then_some(records))
    }

    async fn read_record(&mut self) -> Result<Option<(AthleteRecord, usize)>> {
        if let Some(record) = self.pending_record.take() {
            return Ok(Some(record));
        }
        if self.records_finished {
            return Ok(None);
        }
        let id: Option<i64> = sqlx::query_scalar(include_str!("export_snapshot_scope.sql"))
            .bind(&self.states)
            .bind(&self.years)
            .bind(self.athlete_cursor)
            .fetch_optional(&mut *self.transaction)
            .await?;
        let Some(id) = id else {
            self.records_finished = true;
            return Ok(None);
        };
        self.athlete_cursor = id;
        self.check_components(id).await?;
        let budget = i64::try_from(EXPORT_BATCH_BYTES)?;
        let row: Option<(Option<String>, i64)> =
            sqlx::query_as(sqlx::AssertSqlSafe(Arc::clone(&self.record_sql)))
                .bind(&self.states)
                .bind(&self.years)
                .bind(id)
                .bind(budget)
                .fetch_optional(&mut *self.transaction)
                .await
                .context("loading bounded canonical athlete")?;
        let (document, size) =
            row.context("selected athlete disappeared inside repeatable-read snapshot")?;
        let document = document.ok_or(ExportLimitExceeded {
            kind: "athlete serialized bytes",
            id,
            actual: size,
            limit: budget,
        })?;
        let record =
            serde_json::from_str(&document).context("decoding canonical athlete report")?;
        Ok(Some((record, usize::try_from(size)?)))
    }

    async fn check_components(&mut self, id: i64) -> Result<()> {
        let counts: Vec<(String, i64)> = sqlx::query_as(
            "SELECT 'athlete observations',count(*)::bigint FROM (SELECT r.id FROM results r JOIN athlete_sources p ON p.id=r.athlete_source_id WHERE p.athlete_id=$1 LIMIT 10001) r \
             UNION ALL SELECT 'athlete profiles',count(*)::bigint FROM (SELECT id FROM athlete_sources WHERE athlete_id=$1 LIMIT 10001) p"
        ).bind(id).fetch_all(&mut *self.transaction).await?;
        counts.into_iter().try_for_each(|(kind, count)| {
            ensure!(
                count <= EXPORT_MAX_OBSERVATIONS,
                ExportLimitExceeded {
                    kind: if kind == "athlete observations" {
                        "athlete observations"
                    } else {
                        "athlete profiles"
                    },
                    id,
                    actual: count,
                    limit: EXPORT_MAX_OBSERVATIONS
                }
            );
            Ok::<_, anyhow::Error>(())
        })?;
        let components: Vec<(String, i64, i64)> =
            sqlx::query_as(include_str!("export_snapshot_components.sql"))
                .bind(id)
                .fetch_all(&mut *self.transaction)
                .await?;
        let bytes = components
            .into_iter()
            .try_fold(0_i64, |bytes, (_, count, next_bytes)| {
                ensure!(
                    count <= EXPORT_MAX_OBSERVATIONS,
                    ExportLimitExceeded {
                        kind: "athlete component rows",
                        id,
                        actual: count,
                        limit: EXPORT_MAX_OBSERVATIONS
                    }
                );
                bytes
                    .checked_add(next_bytes)
                    .context("athlete constituent byte counter overflow")
            })?;
        ensure!(
            bytes <= EXPORT_COMPONENT_BYTES,
            ExportLimitExceeded {
                kind: "athlete constituent serialized bytes",
                id,
                actual: bytes,
                limit: EXPORT_COMPONENT_BYTES
            }
        );
        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub async fn next_issues(&mut self) -> Result<Option<Vec<ExportIssue>>> {
        ensure!(self.usable, ExportSnapshotInvalidated);
        self.usable = false;
        let result = self.read_issues_batch().await;
        if result.is_ok() {
            self.usable = true;
        }
        result
    }

    async fn read_issues_batch(&mut self) -> Result<Option<Vec<ExportIssue>>> {
        let (_, issues, _, _) = stream::iter(0..3)
            .map(Ok::<_, anyhow::Error>)
            .try_fold(
                (self, Vec::new(), 0_usize, false),
                |(snapshot, mut issues, bytes, full), _| async move {
                    if full
                        || snapshot.issues_finished
                        || issues.len() == EXPORT_BATCH_ROWS
                        || bytes == EXPORT_BATCH_BYTES
                    {
                        return Ok((snapshot, issues, bytes, full));
                    }
                    let max_rows = EXPORT_BATCH_ROWS
                        .checked_sub(issues.len())
                        .context("coverage row budget underflow")?;
                    let max_bytes = EXPORT_BATCH_BYTES
                        .checked_sub(bytes)
                        .context("coverage byte budget underflow")?;
                    let (batch, batch_bytes, full) =
                        snapshot.fetch_issue_batch(max_rows, max_bytes).await?;
                    issues.extend(batch);
                    let bytes = bytes
                        .checked_add(batch_bytes)
                        .context("coverage byte counter overflow")?;
                    Ok((snapshot, issues, bytes, full))
                },
            )
            .await?;
        Ok((!issues.is_empty()).then_some(issues))
    }

    async fn fetch_issue_batch(
        &mut self,
        max_rows: usize,
        max_bytes: usize,
    ) -> Result<(Vec<ExportIssue>, usize, bool)> {
        let rows: Vec<IssueRow> = sqlx::query_as(include_str!("export_issues.sql"))
            .bind(&self.states)
            .bind(self.issue_cursor.0)
            .bind(self.issue_cursor.1)
            .bind(self.issue_cursor.2)
            .bind(i64::try_from(max_bytes)?)
            .bind(i64::try_from(max_rows)?)
            .fetch_all(&mut *self.transaction)
            .await
            .context("loading bounded coverage issue batch")?;
        let Some(first) = rows.first() else {
            self.advance_issue_phase()?;
            return Ok((Vec::new(), 0, false));
        };
        if first.0.is_none()
            && first.6 > i64::try_from(max_bytes)?
            && first.6 <= i64::try_from(EXPORT_BATCH_BYTES)?
        {
            return Ok((Vec::new(), 0, true));
        }
        let exhausted = first.7;
        let cursor = rows
            .last()
            .map(|row| (row.3, row.4, row.5))
            .context("missing coverage batch cursor")?;
        let (issues, bytes) = decode_issue_batch(rows)?;
        let full = !exhausted && issues.len() < max_rows;
        if exhausted {
            self.advance_issue_phase()?;
        } else {
            self.issue_cursor = cursor;
        }
        Ok((issues, bytes, full))
    }

    fn advance_issue_phase(&mut self) -> Result<()> {
        self.issue_cursor = (
            self.issue_cursor
                .0
                .checked_add(1)
                .context("coverage phase overflow")?,
            0,
            0,
        );
        self.issues_finished = self.issue_cursor.0 > 2;
        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub async fn finish(self) -> Result<()> {
        self.transaction
            .commit()
            .await
            .context("finishing consistent export snapshot")
    }
}

pub(super) async fn collect(
    snapshot: ExportSnapshot<'_>,
) -> Result<(Vec<AthleteRecord>, Vec<ExportIssue>)> {
    let (records, issues, _, _) = parts(snapshot, EXPORT_BATCH_ROWS)
        .try_fold(
            (Vec::new(), Vec::new(), 0_usize, 0_usize),
            |(mut records, mut issues, bytes, rows), (next_records, next_issues)| async move {
                let rows = rows
                    .checked_add(next_records.len())
                    .and_then(|rows| rows.checked_add(next_issues.len()))
                    .context("export collection row counter overflow")?;
                ensure!(
                    rows <= EXPORT_COLLECTION_ROWS,
                    ExportLimitExceeded {
                        kind: "collected snapshot rows",
                        id: 0,
                        actual: i64::try_from(rows)?,
                        limit: i64::try_from(EXPORT_COLLECTION_ROWS)?
                    }
                );
                let bytes = collection_bytes(bytes, &next_records, &next_issues)?;
                ensure!(
                    bytes <= EXPORT_COLLECTION_BYTES,
                    ExportLimitExceeded {
                        kind: "collected snapshot serialized bytes",
                        id: 0,
                        actual: i64::try_from(bytes)?,
                        limit: i64::try_from(EXPORT_COLLECTION_BYTES)?
                    }
                );
                records.extend(next_records);
                issues.extend(next_issues);
                Ok((records, issues, bytes, rows))
            },
        )
        .await?;
    Ok((records, issues))
}

struct SerializedSize(usize);

impl std::io::Write for SerializedSize {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_add(buffer.len())
            .ok_or_else(|| std::io::Error::other("serialized export size overflow"))?;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn collection_bytes(
    bytes: usize,
    records: &[AthleteRecord],
    issues: &[ExportIssue],
) -> Result<usize> {
    let mut size = SerializedSize(bytes);
    records
        .iter()
        .try_for_each(|record| serde_json::to_writer(&mut size, record))?;
    issues.iter().try_fold(size.0, |bytes, issue| {
        bytes
            .checked_add(issue.source.len())
            .and_then(|bytes| bytes.checked_add(issue.url.len()))
            .and_then(|bytes| bytes.checked_add(issue.detail.len()))
            .and_then(|bytes| bytes.checked_add(128))
            .context("export collection byte counter overflow")
    })
}

fn parts(
    snapshot: ExportSnapshot<'_>,
    rows: usize,
) -> impl futures::TryStream<Ok = (Vec<AthleteRecord>, Vec<ExportIssue>), Error = anyhow::Error> {
    stream::try_unfold(snapshot, move |mut snapshot| async move {
        if let Some(records) = snapshot.next_records(rows).await? {
            let issues = snapshot
                .next_issues()
                .await?
                .map_or(Vec::new(), core::convert::identity);
            return Ok(Some(((records, issues), snapshot)));
        }
        if let Some(issues) = snapshot.next_issues().await? {
            return Ok(Some(((Vec::new(), issues), snapshot)));
        }
        snapshot.finish().await?;
        Ok(None)
    })
}

pub(super) async fn write(
    snapshot: ExportSnapshot<'_>,
    output: &Path,
    options: &ExportOptions,
    layout: crate::export::ExportLayout,
) -> Result<Vec<PathBuf>> {
    let staged = StagedExport::new(output, options)?.with_layout(layout);
    let rows = options.rows_per_file.min(EXPORT_BATCH_ROWS);
    let staged = parts(snapshot, rows)
        .try_fold(staged, |mut staged, (records, issues)| async move {
            tokio::task::spawn_blocking(move || {
                staged.stage_part(&records, &issues)?;
                Ok::<_, anyhow::Error>(staged)
            })
            .await
            .context("joining bounded workbook staging")?
        })
        .await?;
    tokio::task::spawn_blocking(move || staged.finish())
        .await
        .context("joining export publication")?
}
