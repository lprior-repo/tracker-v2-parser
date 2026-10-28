use anyhow::{Context as _, Result};
use serde_json::{Value, json};
use track_scrapper_v2::marks::{comparable_performance, normalized_performance};
use track_scrapper_v2::model::{PageContext, PageKind, Source, Sport, Target};
use track_scrapper_v2::sources::athleticnet::{athlete_target, meet_target, parse};

fn bio(definitions: Vec<Value>, kind: Option<i64>) -> Value {
    let mut row = json!({"IDResult":1,"EventID":12,"SchoolID":3204,
        "SeasonID":2026,"Result":"40-00"});
    if let Some(kind) = kind {
        row["EventTypeID"] = json!(kind);
    }
    json!({"athlete":{"IDAthlete":1,"FirstName":"Public","SchoolID":3204},
        "allTeams":{"3204":{"SchoolName":"School"}},
        "allSeasons":[{"SchoolID":3204,"IDSeason":2026,"Display":"2026 Outdoor"}],
        "eventsTF":definitions,"resultsTF":[row],"resultsXC":[]})
}

#[test]
fn given_ambiguous_bio_definitions_when_reordered_then_no_classification_is_guessed() -> Result<()>
{
    let ordinary = json!({"IDEvent":12,"IDEventType":0,"Event":"Shot Put","Description":"12lb"});
    let seated =
        json!({"IDEvent":12,"IDEventType":99,"Event":"Shot Put","Description":"12lb Seated"});
    let weight = json!({"IDEvent":12,"IDEventType":1,"Event":"Shot Put","Description":"16lb"});
    let untyped = json!({"IDEvent":12,"Event":"Shot Put","Description":"4kg"});
    for kind in [None, Some(0)] {
        let mut definitions = if kind.is_none() {
            vec![ordinary.clone(), seated.clone(), weight.clone()]
        } else {
            vec![untyped.clone(), ordinary.clone()]
        };
        for _ in 0..definitions.len() {
            let body = bio(definitions.clone(), kind);
            let page = parse(
                &athlete_target("1", None, Sport::Outdoor)?,
                &body.to_string(),
            )?;
            if kind.is_none() {
                assert!(page.performances.is_empty());
                assert_eq!(page.unassigned_results.len(), 1);
                assert_eq!(page.unassigned_results[0].raw["Result"], "40-00");
            } else {
                assert_eq!(page.performances.len(), 1);
                assert_eq!(page.performances[0].event, "Shot Put (12lb)");
            }
            definitions.rotate_left(1);
        }
    }
    Ok(())
}

#[test]
fn given_unique_untyped_bio_definition_when_parsed_then_published_label_survives() -> Result<()> {
    let body = bio(
        vec![json!({"IDEvent":12,"Event":"Shot Put","Description":"12lb"})],
        None,
    );
    let page = parse(
        &athlete_target("1", None, Sport::Outdoor)?,
        &body.to_string(),
    )?;
    assert_eq!(page.performances.len(), 1);
    assert_eq!(page.performances[0].event, "Shot Put (12lb)");
    Ok(())
}

fn ranking_target() -> Target {
    Target::get(
        Source::AthleticNet,
        PageKind::Directory,
        "https://www.athletic.net/api/v1/tfRankings/GetRankings".into(),
        PageContext {
            sport: Some(Sport::Outdoor),
            acquisition_state: Some("WI".into()),
            ..PageContext::default()
        },
    )
}

fn ranking(name: &str, state: Option<&str>) -> Value {
    json!({"division":{"ID":170770},"gender":"m","eventShort":"100m",
        "groupedRankings":[[{"AthleteID":1,"AthleteName":name,"TeamID":3204,
            "TeamName":"School","State":state,"MeetID":634313,"PersonalEvent":true}]]})
}

#[test]
fn given_masked_ranking_names_when_parsed_then_no_athlete_or_bio_is_created() -> Result<()> {
    for name in ["••••", "****", "Public ••••", "<private>"] {
        let page = parse(&ranking_target(), &ranking(name, None).to_string())?;
        assert!(page.athletes.is_empty(), "{name}");
        assert!(page.next.iter().all(|next| next.kind != PageKind::Athlete));
        assert!(page.notes.iter().any(|note| note.contains("identity")));
    }
    Ok(())
}

#[test]
fn given_ranking_acquisition_scope_when_following_then_scope_stays_separate_from_state()
-> Result<()> {
    for state in [None, Some("IL")] {
        let page = parse(
            &ranking_target(),
            &ranking("Public Runner", state).to_string(),
        )?;
        assert_eq!(page.athletes.len(), 1);
        assert_eq!(page.athletes[0].state.as_deref(), state);
        assert_eq!(page.schools[0].state.as_deref(), state);
        assert_eq!(page.next.len(), 2);
        assert!(
            page.next
                .iter()
                .all(|next| next.context.acquisition_state.as_deref() == Some("WI"))
        );
        let athlete = page
            .next
            .iter()
            .find(|next| next.kind == PageKind::Athlete)
            .context("bio")?;
        assert_eq!(athlete.context.state.as_deref(), state);
    }
    Ok(())
}

#[test]
fn given_captured_hurdle_units_when_compared_then_numeric_finishers_keep_literal_specification()
-> Result<()> {
    let metadata = parse(
        &meet_target("634313", Sport::Outdoor)?,
        include_str!("../fixtures/athleticnet/meet-634313.json"),
    )?;
    let target = metadata.next.first().context("results target")?;
    let page = parse(
        target,
        include_str!("../fixtures/athleticnet/results-634313.json"),
    )?;
    let hurdles: Vec<_> = page
        .performances
        .iter()
        .filter(|p| p.event.contains("Hurdles"))
        .collect();
    assert_eq!(hurdles.len(), 87);
    let finishers: Vec<_> = hurdles
        .into_iter()
        .filter(|p| normalized_performance(p).is_some())
        .collect();
    assert_eq!(finishers.len(), 78);
    for performance in finishers {
        let compared = comparable_performance(Source::AthleticNet, &target.url, performance)
            .with_context(|| format!("{}: {}", performance.event, performance.mark))?;
        assert!(
            compared
                .column
                .ends_with(&format!("::spec={}", performance.event))
        );
        assert!(performance.event.contains("\" / 0."));
    }
    Ok(())
}
