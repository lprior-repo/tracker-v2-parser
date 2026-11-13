use serde::{Deserialize, Serialize, Serializer};

use super::PageRequest;

#[derive(Debug, Deserialize)]
#[serde(from = "SelectionWire")]
pub enum SelectionStep {
    Page(Box<PageRequest>),
    Empty,
    PolicyAdvanced,
    DeadlineReached,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SelectionWire {
    Page(Option<Box<PageRequest>>),
    Progress(ProgressMarker),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProgressMarker {
    selection: MarkerKind,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MarkerKind {
    PolicyAdvanced,
    DeadlineReached,
}

impl From<SelectionWire> for SelectionStep {
    fn from(wire: SelectionWire) -> Self {
        match wire {
            SelectionWire::Page(Some(page)) => Self::Page(page),
            SelectionWire::Page(None) => Self::Empty,
            SelectionWire::Progress(ProgressMarker {
                selection: MarkerKind::PolicyAdvanced,
            }) => Self::PolicyAdvanced,
            SelectionWire::Progress(ProgressMarker {
                selection: MarkerKind::DeadlineReached,
            }) => Self::DeadlineReached,
        }
    }
}

impl Serialize for SelectionStep {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Page(page) => page.serialize(serializer),
            Self::Empty => serializer.serialize_none(),
            Self::PolicyAdvanced => ProgressMarker {
                selection: MarkerKind::PolicyAdvanced,
            }
            .serialize(serializer),
            Self::DeadlineReached => ProgressMarker {
                selection: MarkerKind::DeadlineReached,
            }
            .serialize(serializer),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SelectionStep;
    use crate::durable::page::{PageScope, request};
    use crate::model::{PageContext, PageKind, Source, Target};
    use serde_json::json;

    #[test]
    fn old_page_and_empty_run_payloads_keep_their_wire_representation() -> anyhow::Result<()> {
        let target = Target::get(
            Source::MileSplit,
            PageKind::Meet,
            "https://wi.milesplit.com/meets/900-public-results".into(),
            PageContext::default(),
        );
        let scope = PageScope {
            run_generation: "wire-compatibility".into(),
            config_sha256: "a".repeat(64),
            deadline_ms: 1_000_000,
            cooldown_ms: 60_000,
            inherited_retry_campaigns: Vec::new(),
        };
        let page = request(crate::database::Job { id: 7, target }, &scope)?;
        let historical_bytes = serde_json::to_vec(&Some(&page))?;
        let replayed: SelectionStep = serde_json::from_slice(&historical_bytes)?;
        assert_eq!(serde_json::to_vec(&replayed)?, historical_bytes);
        let previous = serde_json::to_value(Some(&page))?;
        let step: SelectionStep = serde_json::from_value(previous.clone())?;
        assert!(matches!(&step, SelectionStep::Page(p) if p.binding == page.binding));
        assert_eq!(serde_json::to_value(step)?, previous);
        let empty: SelectionStep = serde_json::from_value(json!(null))?;
        assert!(matches!(empty, SelectionStep::Empty));
        assert_eq!(serde_json::to_value(empty)?, json!(null));
        Ok(())
    }

    #[tokio::test]
    async fn real_database_failure_stays_retryable_in_the_native_progress_api() -> anyhow::Result<()>
    {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgresql://localhost/unused_policy_failure")?;
        pool.close().await;
        let database = crate::database::Database::durable_test_database(pool);
        let config = crate::config::Config::parse(
            "states=['WI']\n[acquisition]\nmode='public_results'\n[sources.athleticnet]\nenabled=false",
        )?;
        let now = chrono::Utc::now().timestamp_millis();
        let scope = PageScope {
            run_generation: "policy-failure".into(),
            config_sha256: crate::durable::model::page_config_digest(&config)?,
            deadline_ms: now + 60_000,
            cooldown_ms: 60_000,
            inherited_retry_campaigns: Vec::new(),
        };
        let error = super::super::select_step_with_extension(
            &database,
            &config,
            &scope,
            now,
            &[],
            super::super::Scheduling::Resume,
            None,
        )
        .await
        .expect_err("closed pool must not become policy progress or queue empty");
        assert!(crate::durable::retryable_error(&error));
        Ok(())
    }

    #[test]
    fn policy_progress_is_distinct_from_empty_and_rejects_foreign_payloads() -> anyhow::Result<()> {
        let marker = json!({"selection":"policy_advanced"});
        let step: SelectionStep = serde_json::from_value(marker.clone())?;
        assert!(matches!(step, SelectionStep::PolicyAdvanced));
        assert_eq!(serde_json::to_value(step)?, marker);
        let deadline = json!({"selection":"deadline_reached"});
        let step: SelectionStep = serde_json::from_value(deadline.clone())?;
        assert!(matches!(step, SelectionStep::DeadlineReached));
        assert_eq!(serde_json::to_value(step)?, deadline);
        for invalid in [
            json!({}),
            json!({"selection":"empty"}),
            json!({"selection":"policy_advanced","page_id":7}),
            json!({"binding":null,"target":null}),
            json!([]),
        ] {
            assert!(serde_json::from_value::<SelectionStep>(invalid).is_err());
        }
        Ok(())
    }
}
