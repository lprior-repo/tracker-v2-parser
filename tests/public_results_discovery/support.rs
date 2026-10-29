use anyhow::{Context, Result};
use track_scrapper_v2::{
    config::Config,
    database::Database,
    durable::page::{self, PageRequest, PageScope},
    model::{Capture, PageContext, PageKind, ParsedPage, Source, Target},
    sources,
};

pub fn config(state: &str, policy: &str, source: &str) -> Result<Config> {
    Config::parse(&format!(
        "states=['{state}']\ngraduation_years=[2027]\nseason=2026\n{policy}\n[sources.athleticnet]\nenabled={}\n[sources.milesplit]\nenabled={}\ntransport='http'\n",
        source == "athleticnet",
        source == "milesplit"
    ))
}

pub const PUBLIC: &str =
    "[acquisition]\nmode='public_results'\ncollect_contacts=true\nprofile_coverage_only=false";

pub fn target(kind: PageKind, url: &str, school: Option<&str>, athlete: Option<&str>) -> Target {
    Target::get(
        Source::MileSplit,
        kind,
        url.into(),
        PageContext {
            state: Some("OH".into()),
            school_id: school.map(str::to_owned),
            athlete_id: athlete.map(str::to_owned),
            season: Some(2026),
            ..Default::default()
        },
    )
}

pub fn capture(target: &Target, html: &str) -> Capture {
    Capture {
        url: target.url.clone(),
        status: 200,
        content_type: "text/html".into(),
        body: html.as_bytes().to_vec(),
    }
}

pub async fn done(database: &Database, target: &Target) -> Result<()> {
    database
        .persist(
            None,
            target,
            &capture(target, "<html></html>"),
            &ParsedPage::default(),
        )
        .await?;
    Ok(())
}

pub async fn replay(
    database: &Database,
    root: &std::path::Path,
    scope: &PageScope,
    target: Target,
    html: &str,
) -> Result<()> {
    let response = capture(&target, html);
    assert!(matches!(
        page::offline_replay(database, root, scope, target, response).await?,
        page::PageOutcome::Completed { .. }
    ));
    Ok(())
}

pub async fn selected(
    database: &Database,
    config: &Config,
    scope: &PageScope,
) -> Result<PageRequest> {
    page::select(
        database,
        config,
        scope,
        chrono::Utc::now().timestamp_millis(),
    )
    .await?
    .context("eligible discovery page")
}

pub async fn register(database: &Database, id: &str, year: Option<i32>) -> Result<Target> {
    let target = target(
        PageKind::Athlete,
        &format!("https://oh.milesplit.com/athletes/{id}-runner"),
        None,
        Some(id),
    );
    let canonical: i64 = sqlx::query_scalar(
        "INSERT INTO athletes(name,graduation_year,state) VALUES('Runner',$1,'OH') RETURNING id",
    )
    .bind(year)
    .fetch_one(database.pool())
    .await?;
    sqlx::query("INSERT INTO athlete_sources(athlete_id,source,source_id,profile_url,graduation_year) VALUES($1,'milesplit',$2,$3,$4)").bind(canonical).bind(id).bind(&target.url).bind(year).execute(database.pool()).await?;
    Ok(target)
}

pub async fn snapshot(database: &Database) -> Result<serde_json::Value> {
    Ok(sqlx::query_scalar("SELECT jsonb_build_object('pages',(SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM scrape_pages p),'operations',(SELECT jsonb_agg(to_jsonb(d) ORDER BY operation_id) FROM durable_page_operations d),'attempts',(SELECT jsonb_agg(to_jsonb(d) ORDER BY attempt_id) FROM durable_page_attempts d),'contacts',(SELECT jsonb_agg(to_jsonb(c) ORDER BY id) FROM contacts c))").fetch_one(database.pool()).await?)
}

pub async fn official(database: &Database) -> Result<(Target, Target)> {
    let mut team = target(
        PageKind::Team,
        "https://wi.milesplit.com/teams/52649-abbotsford",
        Some("52649"),
        None,
    );
    team.context.state = Some("WI".into());
    team.context.school_name = Some("Abbotsford".into());
    done(database, &team).await?;
    let mut official = team.clone();
    official.kind = PageKind::Contacts;
    official.url = "https://schools.wiaawi.org/Directory/School/GetDirectorySchool?orgID=1".into();
    let html = include_str!("../../fixtures/contacts/wiaa-abbotsford-detail.html");
    let parsed = sources::wiaa::parse(&official, html)?;
    assert_eq!(parsed.coaches.len(), 4);
    assert!(
        parsed
            .coaches
            .iter()
            .all(|coach| coach.name.is_some() && coach.email.is_some())
    );
    assert!(parsed.schools[0].address.is_some() && parsed.schools[0].postal_code.is_some());
    database
        .persist(None, &official, &capture(&official, html), &parsed)
        .await?;
    let mut web = official;
    web.url = "https://abbotsford.example.edu/staff".into();
    database.enqueue(std::slice::from_ref(&web), false).await?;
    Ok((team, web))
}
