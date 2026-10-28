mod support;

use anyhow::{Context, Result};
use track_scrapper_v2::{
    database::Database,
    marks::{Measure, comparable_performance, normalized_mark},
    model::{Capture, PageContext, PageKind, Source, Sport, Target},
    transport::redact_capture,
};

fn target() -> Target {
    Target::get(
        Source::AthleticNet,
        PageKind::Athlete,
        "https://www.athletic.net/api/v1/AthleteBio/GetAthleteBioData?athleteId=28872883&sport=tf&level=0".into(),
        PageContext {
            athlete_id: Some("28872883".into()),
            state: Some("WI".into()),
            sport: Some(Sport::Outdoor),
            ..PageContext::default()
        },
    )
}

#[tokio::test]
async fn given_invalid_secondary_urls_when_batch_enqueued_then_no_checkpoint_changes() -> Result<()>
{
    support::with_schema(|test| {
        Box::pin(async move {
            let database = Database::connect(&test.url).await?;
            let valid = target();
            database
                .enqueue(std::slice::from_ref(&valid), false)
                .await?;
            sqlx::query("UPDATE scrape_pages SET status='succeeded',attempts=2")
                .execute(&test.pool)
                .await?;
            for secondary in [
                "https://www.athletic.net/core?token=synthetic-canary",
                "https://elsewhere.example/core",
                "https://user:synthetic-canary@www.athletic.net/core",
                "file:///etc/hosts",
                "http://www.athletic.net/core",
            ] {
                for browser in [false, true] {
                    let mut invalid = valid.clone();
                    invalid.url.push_str("&page=2");
                    if browser {
                        invalid.browser_origin = Some(secondary.into());
                    } else {
                        invalid.token_url = Some(secondary.into());
                    }
                    assert!(
                        database
                            .enqueue(&[valid.clone(), invalid], true)
                            .await
                            .is_err()
                    );
                    let state: Vec<(String, String, i32)> =
                        sqlx::query_as("SELECT url,status,attempts FROM scrape_pages ORDER BY id")
                            .fetch_all(&test.pool)
                            .await?;
                    assert_eq!(state, vec![(valid.url.clone(), "succeeded".into(), 2)]);
                }
            }
            database.close().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn given_retained_athleticnet_html_challenge_when_replayed_then_blocked_state_persists()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        let input = test.directory().join("challenge.html");
        std::fs::write(&input, "\u{feff} \n<!doctype html><title>Just a moment</title><main>Verify you are human</main>")?;
        let destination = target();
        let args = [
            "scrape", "--source", "athleticnet", "--kind", "athlete",
            "--url", destination.url.as_str(), "--input", input.to_str().context("challenge path")?,
            "--athlete-id", "28872883", "--state", "WI", "--sport", "outdoor",
        ];
        let first = test.cli(&args).await?;
        assert!(!first.status.success());
        let state: (String, i32, bool) = sqlx::query_as(
            "SELECT status,attempts,last_success IS NOT NULL FROM scrape_pages"
        ).fetch_one(&test.pool).await?;
        assert_eq!(state, ("blocked".into(), 1, false));
        let second = test.cli(&args).await?;
        assert!(!second.status.success());
        let after: (String, i32, bool) = sqlx::query_as(
            "SELECT status,attempts,last_success IS NOT NULL FROM scrape_pages"
        ).fetch_one(&test.pool).await?;
        assert_eq!(after, state);
        let results: i64 = sqlx::query_scalar("SELECT count(*) FROM results")
            .fetch_one(&test.pool).await?;
        assert_eq!(results, 0);
        Ok(())
    })).await
}

#[test]
fn given_bracket_sensitive_assignments_when_sanitized_then_no_canary_remains() -> Result<()> {
    for assignment in [
        r#"window["token"] = "synthetic-canary";"#,
        "window['authorization'] = 'synthetic-canary';",
        r#"window["to\u006ben"] = "synthetic-canary";"#,
        "window['token' /* key */] /* operator */ = /* value */ 'synthetic-canary';",
        "window.token // key\n = 'synthetic-canary';",
    ] {
        let body = format!("<p>Public result 44.88</p><script>{assignment}</script>");
        let capture = Capture {
            url: "https://www.athletic.net/results".into(),
            status: 200,
            content_type: "text/html".into(),
            body: body.into_bytes(),
        };
        let retained = redact_capture(capture)?;
        let text = std::str::from_utf8(&retained.body)?;
        assert!(!text.contains("synthetic-canary"));
        assert!(text.contains("Public result 44.88"));
    }
    Ok(())
}

#[test]
fn given_unsupported_sensitive_scripts_when_sanitized_then_scripts_are_removed_without_losing_public_data()
-> Result<()> {
    for assignment in [
        "window.token = 'synthetic-' + 'canary';",
        r#"window["token"] = "synthetic-" + "canary";"#,
        "window.token += 'synthetic-canary';",
        "window.token = getToken('synthetic-canary');",
        "window.token = `synthetic-canary`;",
        "window.token **= 'synthetic-canary';",
        "window.token <<= 'synthetic-canary';",
        "window['token'] &&= 'synthetic-canary';",
        "window['authorization'] >>= 'synthetic-canary';",
    ] {
        let capture = Capture {
            url: "https://www.athletic.net/results".into(),
            status: 200,
            content_type: "text/html".into(),
            body: format!("<p>Public result 44.88</p><script>{assignment}</script><script>{assignment}</script><script type='application/ld+json'>{{\"name\":\"Public Athlete\"}}</script>").into_bytes(),
        };
        let retained = redact_capture(capture)?;
        let text = std::str::from_utf8(&retained.body)?;
        assert!(!text.contains("synthetic"));
        assert!(!text.contains("window"));
        assert!(text.contains("Public result 44.88"));
        assert!(text.contains("{\"name\":\"Public Athlete\"}"));
    }
    Ok(())
}

#[test]
fn given_ambiguous_computed_or_outside_script_assignments_when_sanitized_then_no_capture_is_published()
 {
    let long_computed = format!(
        "<script>window['to' + {}'ken'] = 'synthetic-canary';</script>",
        " ".repeat(1_025)
    );
    let padded_prefix = format!(
        "<script>window[/* key */{}((('to') /* prefix */ + 'ken'))]='synthetic-canary';</script>",
        " ".repeat(1_025)
    );
    for body in [
        "<script>window['se' + 'cret'] = 'synthetic-canary';</script>",
        "<script>window[`token`] = 'synthetic-canary';</script>",
        "<script>window[`to` + `ken`] = 'synthetic-canary';</script>",
        "<script>window['to' /* key */ + 'ken'] = 'synthetic-canary';</script>",
        "<script>window['to' +\n'ken'] = 'synthetic-canary';</script>",
        "<script>window[('to' + 'ken')] = 'synthetic-canary';</script>",
        "<script>window[/* key */'to' + 'ken']='synthetic-canary';</script>",
        "<script>window[(('to' + 'ken'))]='synthetic-canary';</script>",
        "<script>window[((('to')) + 'ken')]='synthetic-canary';</script>",
        "<script>window[// key\n(('to') // prefix\n+ 'ken')]='synthetic-canary';</script>",
        "<script>window[/* key */((`token`))] ||= 'synthetic-canary';</script>",
        "<script>window[(`to${suffix}`)] **= 'synthetic-canary';</script>",
        "<script>window[(('to' + 'ken'))] += 'synthetic-canary';</script>",
        "<script>const results={\"Result\":\"44.88\"}; window[/* key */(('to') + 'ken')]='synthetic-canary';</script>",
        long_computed.as_str(),
        padded_prefix.as_str(),
        "<script>const results={\"Result\":\"44.88\"}; stats['event' + suffix] = 1;</script>",
        "<p>token='a'+'b';</p>\"<script>token='a'+'b';</script>\"=1",
        "<p>token='synthetic-'+'canary';</p><script>token='synthetic-'+'canary';</script>",
        "<script>window[/* key */((`to` /* prefix */ + `ken`))]='synthetic-canary';</script>",
        "<p>window[(('to' + 'ken'))]='synthetic-canary';</p><script>window[(('to' + 'ken'))]='synthetic-canary';</script>",
    ] {
        let failure = redact_capture(Capture {
            url: "https://www.athletic.net/results".into(),
            status: 200,
            content_type: "text/html".into(),
            body: body.as_bytes().to_vec(),
        })
        .expect_err("ambiguous sensitive content must not be retained");
        assert!(!format!("{failure:#}").contains("synthetic-canary"));
    }
}

#[test]
fn given_excessive_capture_or_script_count_when_sanitized_then_input_is_rejected_before_retention()
{
    for body in [
        vec![b' '; 16 * 1024 * 1024 + 1],
        "<script></script>".repeat(4_097).into_bytes(),
    ] {
        assert!(
            redact_capture(Capture {
                url: "https://www.athletic.net/results".into(),
                status: 200,
                content_type: "text/html".into(),
                body,
            })
            .is_err()
        );
    }
}

#[test]
fn given_decimal_event_distance_when_normalized_then_distance_is_preserved_or_rejected() {
    for label in ["1.0k", "1.00km", "1.5k", "5.0k", "1.0 mile"] {
        assert!(normalized_mark(label, "3:00").is_none(), "{label}");
    }
    assert_eq!(
        normalized_mark("1k", "3:00"),
        Some((180_000_000_000, Measure::Time, None))
    );
    assert_eq!(
        normalized_mark("10k", "30:00"),
        Some((1_800_000_000_000, Measure::Time, None))
    );
    let mut performance = track_scrapper_v2::model::Performance {
        athlete_id: "1".into(),
        event: "1.0k".into(),
        mark: "3:00".into(),
        reported_status: None,
        source_values: None,
        sport: Sport::Outdoor,
        result_id: Some("published-1".into()),
        source_row: None,
        school_id: None,
        meet_id: None,
        meet_name: None,
        date: None,
        wind: None,
        timing: None,
        course: None,
        is_personal_record: false,
        is_relay: false,
        season: None,
        grade: None,
        gender: None,
    };
    assert!(
        comparable_performance(
            Source::MileSplit,
            "https://www.milesplit.com/results",
            &performance
        )
        .is_none()
    );
    performance.event = "1000m".into();
    let classified = comparable_performance(
        Source::MileSplit,
        "https://www.milesplit.com/results",
        &performance,
    )
    .expect("explicit 1000m");
    assert!(classified.column.contains("::1000m::"));
}

#[test]
fn given_published_competition_specs_when_comparing_then_categories_and_decimal_weights_stay_distinct()
 {
    let make = |event: &str, mark: &str| {
        let performance = track_scrapper_v2::model::Performance {
            athlete_id: "1".into(),
            event: event.into(),
            mark: mark.into(),
            reported_status: None,
            source_values: None,
            sport: Sport::Outdoor,
            result_id: Some("1".into()),
            source_row: None,
            school_id: None,
            meet_id: None,
            meet_name: None,
            date: None,
            wind: None,
            timing: None,
            course: None,
            is_personal_record: false,
            is_relay: false,
            season: None,
            grade: None,
            gender: None,
        };
        comparable_performance(
            Source::AthleticNet,
            "https://www.athletic.net/results",
            &performance,
        )
        .expect("explicit published category and specification")
    };
    assert_ne!(
        make("100 Meters (Wheelchair)", "15.00").column,
        make("100m", "15.00").column
    );
    assert_ne!(
        make("Shot Put (12lb Seated)", "10m").column,
        make("Shot Put (12lb)", "10m").column
    );
    assert_ne!(
        make("Discus (1.6kg)", "30m").column,
        make("Discus (16kg)", "30m").column
    );
    assert_eq!(make("Discus (1.6kg)", "30m").value, 30_000_000);
}
