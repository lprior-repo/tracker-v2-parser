use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde_json::Value;
use sqlx::PgPool;
use tracing::{
    Event, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::{Layer, layer::Context};
use track_scrapper_v2::{
    database::Database,
    model::{PageContext, PageKind, ParsedPage, School, Source, Target},
};

#[derive(Clone, Default)]
pub struct Queries(Arc<Mutex<Vec<String>>>);

struct Query(String);

impl Visit for Query {
    fn record_str(&mut self, field: &Field, value: &str) {
        if matches!(field.name(), "db.statement" | "summary") {
            self.0.push_str(value);
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if matches!(field.name(), "db.statement" | "summary") {
            self.0.push_str(&format!("{value:?}"));
        }
    }
}

impl<S> Layer<S> for Queries
where
    S: Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, context: Context<'_, S>) {
        if event.metadata().target() == "sqlx::query"
            && context.event_scope(event).is_some_and(|scope| {
                scope
                    .from_root()
                    .any(|span| span.metadata().name() == "persistence_query_observation")
            })
        {
            let mut query = Query(String::new());
            event.record(&mut query);
            self.0
                .lock()
                .expect("query observation mutex")
                .push(query.0);
        }
    }
}

impl Queries {
    pub fn count(&self, prefix: &str) -> usize {
        self.0
            .lock()
            .expect("query observation mutex")
            .iter()
            .filter(|sql| sql.contains(prefix))
            .count()
    }
}

pub async fn profiles(database: &Database, target: &Target) -> Result<()> {
    let mut first = crate::records::athlete("11");
    first.school_id = Some("101".into());
    let mut second = crate::records::athlete("22");
    second.school_id = Some("102".into());
    crate::records::persist(
        database,
        target,
        &ParsedPage {
            athletes: vec![first, second],
            schools: vec![
                School {
                    id: "101".into(),
                    name: "First School".into(),
                    ..Default::default()
                },
                School {
                    id: "102".into(),
                    name: "Second School".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        },
    )
    .await
}

pub fn results(count: usize) -> ParsedPage {
    ParsedPage {
        performances: (0..count)
            .map(|index| {
                let mut row = crate::records::performance("11.11");
                row.athlete_id = if index % 2 == 0 { "11" } else { "22" }.into();
                row.school_id = Some(if index % 2 == 0 { "101" } else { "102" }.into());
                row.result_id = Some(format!("batch-{index}"));
                row
            })
            .collect(),
        ..Default::default()
    }
}

pub async fn result_ids(pool: &PgPool) -> Result<Vec<(i64, String, String, String, bool)>> {
    Ok(sqlx::query_as("SELECT r.id,r.source_result_id,p.source_id,s.source_id,r.athlete_id=p.athlete_id FROM results r JOIN athlete_sources p ON p.id=r.athlete_source_id JOIN schools s ON s.id=r.represented_school_id ORDER BY r.id")
        .fetch_all(pool).await?)
}

pub fn team(id: usize) -> Target {
    Target::get(
        Source::MileSplit,
        PageKind::Team,
        format!("https://mi.milesplit.com/teams/{id}"),
        PageContext {
            school_id: Some(id.to_string()),
            state: Some("MI".into()),
            ..Default::default()
        },
    )
}

pub fn targets() -> Vec<Target> {
    let mut targets = (0..260).map(team).collect::<Vec<_>>();
    targets.insert(5, team(3));
    targets.insert(130, team(128));
    targets
}

pub fn contact(state: &str) -> Target {
    Target::get(
        Source::MileSplit,
        PageKind::Contacts,
        "https://school.example.edu/staff".into(),
        PageContext {
            school_id: Some("published".into()),
            state: Some(state.into()),
            ..Default::default()
        },
    )
}

pub async fn install_statement_counter(pool: &PgPool) -> Result<()> {
    sqlx::raw_sql("CREATE TABLE batch_statements(calls bigint NOT NULL); INSERT INTO batch_statements VALUES(0); CREATE FUNCTION note_batch_statement() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN UPDATE batch_statements SET calls=calls+1; RETURN NULL; END $$; CREATE TRIGGER batch_statement AFTER INSERT ON scrape_pages FOR EACH STATEMENT EXECUTE FUNCTION note_batch_statement();")
        .execute(pool).await?;
    Ok(())
}

pub async fn statements(pool: &PgPool) -> Result<i64> {
    Ok(sqlx::query_scalar("SELECT calls FROM batch_statements")
        .fetch_one(pool)
        .await?)
}

pub async fn seed_stop(database: &Database, pool: &PgPool, target: &Target) -> Result<()> {
    database
        .enqueue(std::slice::from_ref(target), false)
        .await?;
    sqlx::query("UPDATE scrape_pages SET status='blocked',attempts=3,last_error='retained access stop',cursor=cursor||jsonb_build_object('access_stop_url','https://mi.milesplit.com'),notes=ARRAY['retained'] WHERE url=$1")
        .bind(&target.url).execute(pool).await?;
    Ok(())
}

pub async fn queue_snapshot(pool: &PgPool) -> Result<Vec<Value>> {
    Ok(
        sqlx::query_scalar("SELECT to_jsonb(p)-'created_at' FROM scrape_pages p ORDER BY id")
            .fetch_all(pool)
            .await?,
    )
}

pub async fn sequence(pool: &PgPool) -> Result<i64> {
    Ok(
        sqlx::query_scalar("SELECT last_value FROM scrape_pages_id_seq")
            .fetch_one(pool)
            .await?,
    )
}
