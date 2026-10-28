mod support;

use std::path::Path;

use anyhow::Result;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use track_scrapper_v2::{database::Database, wiaa_coop_import};

const REPORT: &str = "https://schools.wiaawi.org/Reports/RunCoopListReport/";
const HOST: &str = "https://schools.wiaawi.org/Directory/School/GetDirectorySchool?orgID=42";

fn import<'a>(
    url: &'a str,
    path: &'a Path,
) -> futures::future::BoxFuture<'a, Result<wiaa_coop_import::Report>> {
    Box::pin(wiaa_coop_import::import(url, path))
}

fn evidence(directory: &Path, name: &str, body: &str, url: &str, method: &str) -> Result<Value> {
    let path = directory.join(name);
    std::fs::write(&path, body)?;
    Ok(
        json!({"url":url,"method":method,"status":200,"bytes":body.len(),
        "sha256":format!("{:x}", Sha256::digest(body.as_bytes())),
        "captured_utc":"2026-10-28T12:00:00Z","artifact_path":path}),
    )
}

fn fixture(directory: &Path) -> Result<Value> {
    let fields = [
        ("Level", "High School"),
        ("Address", "42 Host Road"),
        ("City", "Host City"),
        ("State", "WI"),
        ("Zip", "54405"),
    ]
    .map(|(label, value)| {
        format!("<div class='entryrowPadding'><span>{label}</span><h5>{value}</h5></div>")
    })
    .join("");
    let coaches = [("Boys Track and Field", "Boy Runner", "boy@example.edu"),
        ("Girls Track and Field", "Girl Runner", "girl@example.edu"),
        ("Girls Cross Country", "Wrong Sport", "wrong@example.edu")]
        .map(|(program,name,email)| format!("<tr><td></td><td>{program}</td><td><label class='formLabelsText'>{name}</label></td><td>Head Coach</td><td><a href='mailto:{email}'>{email}</a></td></tr>")).join("");
    let body = format!(
        "<main id='SchoolItem'><label class='JumboMain'>Example</label><div id='schoolInfo'>{fields}</div><input name='orgID' value='42'><table id='tblCoachList'><tbody>{coaches}</tbody></table></main>"
    );
    let host = evidence(directory, "host.html", &body, HOST, "GET")?;
    let reports = [("M","TR","Boys","Track and Field","Spring"),
        ("F","TR","Girls","Track and Field","Spring"),
        ("M","XC","Boys","Cross Country","Fall"),
        ("F","XC","Girls","Cross Country","Fall")]
        .into_iter().map(|(gender,sport,label,discipline,season)| {
            let coop = if sport == "TR" { "Example/Partner" } else { "Different/Partner" };
            let row = format!("<div class='row gridRowStyle'><div><label>{coop}</label></div><label>{discipline}</label><label>{label}</label><label>{season}</label><label>3</label><label>Example</label><label>2028</label><label>5/1/2028</label><label>200</label></div>");
            let mut value = evidence(directory, &format!("{gender}-{sport}.html"), &row, REPORT, "POST")?;
            value["request_form"] = json!({"YearSel":"2026","GenderSel":gender,"SportSel":sport,"DivisionSel":"-1","Levels":"1"});
            Ok(value)
        }).collect::<Result<Vec<_>>>()?;
    Ok(
        json!({"schema_revision":1,"school_year":"2026-2027","program_reports":reports,
        "associations":[{"provider_source":"milesplit","provider_school_id":"coop-123",
            "provider_school_name":"Example-Partner","provider_school_state":"WI",
            "published_cooperative_name":"Example/Partner","designated_contact_school":"Example",
            "host_wiaa_org_id":"42","host_capture":host}]}),
    )
}

fn save(directory: &Path, manifest: &Value) -> Result<std::path::PathBuf> {
    let path = directory.join("manifest.json");
    std::fs::write(&path, serde_json::to_vec(manifest)?)?;
    Ok(path)
}

async fn seed(test: &support::Scenario) -> Result<()> {
    Database::connect(&test.url).await?.close().await;
    sqlx::query("INSERT INTO schools(source,source_id,name,state,city,address,postal_code,website,source_url) VALUES('milesplit','coop-123','Example-Partner','WI','Original City','10 Cooperative Road','55555','https://example.edu','https://wi.milesplit.com/teams/123')")
        .execute(&test.pool).await?;
    Ok(())
}

#[tokio::test]
async fn endorsed_programs_replay_without_duplicates_or_overwriting_cooperative_identity()
-> Result<()> {
    support::with_schema(|test| {
        Box::pin(async move {
            seed(test).await?;
            let path = save(test.directory(), &fixture(test.directory())?)?;
            let first = import(&test.url, &path).await?;
            assert_eq!(
                (
                    first.associations_imported,
                    first.programs_imported,
                    first.coaches_imported
                ),
                (1, 2, 2)
            );
            assert!(first.unconfirmed_programs.is_empty());
            let again = import(&test.url, &path).await?;
            assert_eq!(again.manifest_sha256, first.manifest_sha256);
            let coaches: Vec<(String, String, Value)> =
                sqlx::query_as("SELECT name,email,raw FROM contacts ORDER BY name")
                    .fetch_all(&test.pool)
                    .await?;
            assert_eq!(coaches.len(), 2);
            assert_eq!(coaches[0].1, "boy@example.edu");
            assert_eq!(coaches[1].1, "girl@example.edu");
            assert!(
                coaches
                    .iter()
                    .all(|(_, _, raw)| !raw.to_string().contains("not confirmed by latest parse"))
            );
            assert!(
                coaches
                    .iter()
                    .all(|(_, _, raw)| raw.to_string().contains("SHA-256"))
            );
            let identity: (String, String, String, String, String) =
                sqlx::query_as("SELECT name,city,address,postal_code,website FROM schools")
                    .fetch_one(&test.pool)
                    .await?;
            assert_eq!(
                identity,
                (
                    "Example-Partner".into(),
                    "Original City".into(),
                    "10 Cooperative Road".into(),
                    "55555".into(),
                    "https://example.edu".into()
                )
            );
            let checkpoints: i64 =
                sqlx::query_scalar("SELECT count(*) FROM scrape_pages WHERE status='succeeded'")
                    .fetch_one(&test.pool)
                    .await?;
            assert_eq!(checkpoints, 1);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn proof_mismatches_and_access_stops_leave_database_unchanged() -> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        seed(test).await?;
        let original = fixture(test.directory())?;
        for (pointer, replacement) in [
            ("/program_reports/0/sha256",json!("bad")),
            ("/program_reports/0/request_form/YearSel",json!("2025")),
            ("/program_reports/0/request_form/SportSel",json!("XC")),
            ("/program_reports/0/request_form/GenderSel",json!("F")),
            ("/associations/0/host_wiaa_org_id",json!("43")),
            ("/associations/0/provider_school_name",json!("Wrong-Partner")),
            ("/associations/0/designated_contact_school",json!("Wrong Host")),
        ] {
            let mut manifest = original.clone();
            *manifest.pointer_mut(pointer).expect("fixture pointer") = replacement;
            let path = save(test.directory(), &manifest)?;
            assert!(import(&test.url, &path).await.is_err(),"{pointer}");
        }
        let contacts: i64 = sqlx::query_scalar("SELECT count(*) FROM contacts").fetch_one(&test.pool).await?;
        let pages: i64 = sqlx::query_scalar("SELECT count(*) FROM scrape_pages").fetch_one(&test.pool).await?;
        assert_eq!((contacts,pages),(0,0));
        let path = save(test.directory(), &original)?;
        sqlx::query("UPDATE schools SET name='Different Cooperative'").execute(&test.pool).await?;
        let before_identity: Value = sqlx::query_scalar("SELECT to_jsonb(s) FROM schools s").fetch_one(&test.pool).await?;
        let error = import(&test.url, &path).await.expect_err("database identity must match");
        assert!(error.to_string().contains("conflicts with existing provider school identity"));
        let after_identity: Value = sqlx::query_scalar("SELECT to_jsonb(s) FROM schools s").fetch_one(&test.pool).await?;
        assert_eq!(before_identity,after_identity);
        sqlx::query("UPDATE schools SET name='Example-Partner'").execute(&test.pool).await?;
        sqlx::query("INSERT INTO scrape_pages(source,page_key,url,cursor,status,last_error) VALUES('milesplit',repeat('a',64),$1,'{\"kind\":\"contacts\"}','blocked','preserved challenge')").bind(HOST).execute(&test.pool).await?;
        let before: Value = sqlx::query_scalar("SELECT to_jsonb(p) FROM scrape_pages p").fetch_one(&test.pool).await?;
        assert!(import(&test.url,&path).await.is_err());
        let after: Value = sqlx::query_scalar("SELECT to_jsonb(p) FROM scrape_pages p").fetch_one(&test.pool).await?;
        assert_eq!(before,after);
        Ok(())
    })).await
}

#[tokio::test]
async fn current_schedule_confirms_uncertain_program_without_reinterpreting_expiration()
-> Result<()> {
    support::with_schema(|test| Box::pin(async move {
        seed(test).await?;
        let mut manifest = fixture(test.directory())?;
        let report = manifest["program_reports"][0].clone();
        let body = std::fs::read_to_string(report["artifact_path"].as_str().unwrap())?.replace("2028","2026");
        let mut updated = evidence(test.directory(), "uncertain-report.html", &body, REPORT, "POST")?;
        updated["request_form"] = report["request_form"].clone();
        manifest["program_reports"][0] = updated;
        let path = save(test.directory(), &manifest)?;
        let unconfirmed = import(&test.url,&path).await?;
        assert_eq!((unconfirmed.coaches_imported, unconfirmed.unconfirmed_programs.len()),(1,1));
        let fields = [("Head Coach","Boy Runner"),("Team Level","Varsity"),("IsCoop","Yes (Example)"),
            ("School Year","2026-2027"),("Gender","Boys"),("Sport","Track and Field")]
            .map(|(label,value)| format!("<div><label>{label}</label><h5>{value}</h5></div>")).join("");
        let body = format!("<div class='jumbotron'><h3>Example/Partner</h3></div><input name='Options.SchoolID' value='42'><input name='Options.SchoolYear' value='2026'><input name='Options.TeamID' value='101'><input name='TeamInfo.TeamID' value='101'><div class='row'>{fields}</div>");
        let url = "https://schools.wiaawi.org/Directory/Schedule/Index?TeamID=101";
        let schedule = evidence(test.directory(),"schedule.html",&body,url,"GET")?;
        manifest["associations"][0]["current_schedules"] = json!([schedule]);
        let path = save(test.directory(),&manifest)?;
        let confirmed = import(&test.url,&path).await?;
        assert_eq!((confirmed.coaches_imported,confirmed.programs_imported),(2,2));
        assert!(confirmed.unconfirmed_programs.is_empty());
        let raw: String = sqlx::query_scalar("SELECT raw::text FROM contacts WHERE email='boy@example.edu'").fetch_one(&test.pool).await?;
        assert!(raw.contains("Expire Yr 2026"));
        assert!(raw.contains("current 2026-2027 schedule"));
        let before: Value = sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(c) ORDER BY id) FROM contacts c").fetch_one(&test.pool).await?;
        for (from,to) in [("2026-2027","2025-2026"),("<h5>Boys</h5>","<h5>Girls</h5>"),
            ("<h5>Track and Field</h5>","<h5>Cross Country</h5>"),
            ("<h3>Example/Partner</h3>","<h3>Other/Partner</h3>"),
            ("value='42'","value='43'"),("Yes (Example)","No"),("Boy Runner","Other Runner")] {
            let invalid = evidence(test.directory(),"bad-schedule.html",&body.replace(from,to),url,"GET")?;
            manifest["associations"][0]["current_schedules"] = json!([invalid]);
            let path = save(test.directory(),&manifest)?;
            assert!(import(&test.url,&path).await.is_err(),"{from}");
        }
        let after: Value = sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(c) ORDER BY id) FROM contacts c").fetch_one(&test.pool).await?;
        assert_eq!(before,after);
        Ok(())
    })).await
}
