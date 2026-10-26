use anyhow::Result;
use track_scrapper_v2::database::Database;
use track_scrapper_v2::model::{
    Athlete, Capture, PageContext, PageKind, ParsedPage, Performance, Source, Sport, Target,
};

pub fn target(source: Source, id: &str) -> Target {
    let host = match source {
        Source::AthleticNet => "https://www.athletic.net/athlete/",
        Source::MileSplit => "https://www.milesplit.com/athletes/",
    };
    Target::get(
        source,
        PageKind::Athlete,
        format!("{host}{id}"),
        PageContext::default(),
    )
}

pub fn athlete(id: &str) -> Athlete {
    Athlete {
        id: id.to_owned(),
        name: "Alex Morgan".to_owned(),
        state: Some("WI".to_owned()),
        graduation_year: Some(2027),
        ..Athlete::default()
    }
}

pub async fn persist(database: &Database, target: &Target, page: &ParsedPage) -> Result<()> {
    let capture = Capture {
        url: target.url.clone(),
        status: 200,
        content_type: "application/json".to_owned(),
        body: serde_json::to_vec(page)?,
    };
    database.persist(None, target, &capture, page).await?;
    Ok(())
}

pub fn performance(mark: &str) -> Performance {
    Performance {
        athlete_id: "11".to_owned(),
        event: "100 Meters".to_owned(),
        mark: mark.to_owned(),
        sport: Sport::Outdoor,
        result_id: Some("race-1".to_owned()),
        source_row: None,
        school_id: None,
        meet_id: Some("meet-1".to_owned()),
        meet_name: Some("County Championship".to_owned()),
        date: Some("2026-05-15".to_owned()),
        wind: Some(0.8),
        timing: Some("FAT".to_owned()),
        course: None,
        is_personal_record: true,
        is_relay: false,
        season: Some(2026),
        grade: Some("11".to_owned()),
        gender: Some("M".to_owned()),
    }
}
