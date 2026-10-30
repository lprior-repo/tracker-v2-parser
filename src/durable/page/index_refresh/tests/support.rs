use crate::{
    config::Config,
    database::Database,
    durable::{
        model,
        operations::{OperationSpec, Workspace},
        pacing::test_support::WireFixture,
        page::PageScope,
    },
    model::{Capture, PageContext, PageKind, Source, Target},
    transport::{Mode, Options, Transport},
};
use anyhow::Result;
use serde_json::Value;

const ROOT_BODY: &str = concat!(
    "<script>const action='/Directory/School/DirectoryLetter?LetterBtn=';</script><select id='Letter'>",
    "<option value='A'>A</option><option value='B'>B</option><option value='C'>C</option><option value='D'>D</option><option value='E'>E</option><option value='F'>F</option><option value='G'>G</option><option value='H'>H</option><option value='I'>I</option><option value='J'>J</option><option value='K'>K</option><option value='L'>L</option><option value='M'>M</option><option value='N'>N</option><option value='O'>O</option><option value='P'>P</option><option value='Q'>Q</option><option value='R'>R</option><option value='S'>S</option><option value='T'>T</option><option value='U'>U</option><option value='V'>V</option><option value='W'>W</option><option value='X'>X</option><option value='Y'>Y</option><option value='Z'>Z</option></select>"
);
const LETTER_BODY: &str = "<div id='ajaxMsgText'>Nothing was found matching this search criteria</div><input id='SearchLetter' value='A'><input id='SearchType' value='1'>";

pub(super) fn config(years: &str) -> Result<Config> {
    Config::parse(&format!(
        "states=['WI']\ngraduation_years=[{years}]\n[acquisition]\nmode='public_results'\ncollect_contacts=true\n[sources.athleticnet]\nenabled=false\n[sources.milesplit]\ntransport='http'"
    ))
}

pub(super) fn scope(
    root: &std::path::Path,
    config: &Config,
    name: &str,
    now: i64,
) -> Result<PageScope> {
    let operation = OperationSpec::WiaaDirectoryRun {
        config: config.clone(),
        deadline_ms: now + 3_600_000,
        cooldown_ms: 15_000,
    };
    let workspace = Workspace::open(&root.join("operations"), name, &operation)?;
    Ok(PageScope {
        run_generation: model::digest(&(workspace.binding(), "wiaa-directory"))?,
        config_sha256: model::page_config_digest(config)?,
        deadline_ms: now + 3_600_000,
        cooldown_ms: 15_000,
    })
}

pub(super) fn target(letter: bool) -> Target {
    let mut target = Target::get(
        Source::MileSplit,
        PageKind::Contacts,
        if letter {
            "https://schools.wiaawi.org/Directory/School/DirectoryLetter?LetterBtn=A"
        } else {
            "https://schools.wiaawi.org/Directory/School/List"
        }
        .into(),
        PageContext {
            state: Some("WI".into()),
            season: Some(2026),
            subject_id: Some("wiaa-directory-index".into()),
            ..Default::default()
        },
    );
    if letter {
        target.method = Some("POST".into());
    }
    target
}

pub(super) const fn body(letter: bool) -> &'static str {
    if letter { LETTER_BODY } else { ROOT_BODY }
}

pub(super) fn capture(target: &Target, body: &str) -> Capture {
    Capture {
        url: target.url.clone(),
        status: 200,
        content_type: "text/html".into(),
        body: body.as_bytes().to_vec(),
    }
}

pub(super) fn connection(
    root: &std::path::Path,
    fixture: &WireFixture,
    database: &Database,
    address: &str,
) -> Result<Transport> {
    let logical = url::Url::parse(address)?;
    let mut wire = fixture.url.clone();
    wire.set_path(logical.path());
    wire.set_query(logical.query());
    Ok(Transport::new(Options {
        mode: Mode::Http,
        driver_url: None,
        profile: root.join("unused"),
        headless: true,
        rps: 3.0,
    })?
    .with_persistent_pacing(database.pool().clone())
    .with_http_fixture("https://schools.wiaawi.org", wire)?)
}

pub(super) async fn operation(database: &Database, id: &str) -> Result<Value> {
    Ok(sqlx::query_scalar(
        "SELECT to_jsonb(d) FROM durable_page_operations d WHERE operation_id=$1",
    )
    .bind(id)
    .fetch_one(database.pool())
    .await?)
}

pub(super) async fn snapshot(database: &Database) -> Result<Value> {
    Ok(sqlx::query_scalar("SELECT jsonb_build_object('pages',(SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM scrape_pages p),'operations',(SELECT jsonb_agg(to_jsonb(d) ORDER BY operation_id) FROM durable_page_operations d),'attempts',(SELECT jsonb_agg(to_jsonb(a) ORDER BY attempt_id) FROM durable_page_attempts a),'receipts',(SELECT jsonb_agg(to_jsonb(r) ORDER BY operation_id) FROM durable_page_receipts r),'wire',(SELECT jsonb_agg(to_jsonb(w) ORDER BY wire_id) FROM durable_wire_requests w))").fetch_one(database.pool()).await?)
}
