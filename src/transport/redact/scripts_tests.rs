use anyhow::Result;

use super::super::body;

const PREFIX: &str = "(window.NREUM||(NREUM={})).init={privacy:{cookies_enabled:true}};(window.NREUM||(NREUM={})).loader_config={agentID:'synthetic-public-id'};;/*! For license information please see nr-loader-spa-1.324.0.min.js.LICENSE.txt */";
const LOADER: &str = r#"(()=>{(()=>{record(["API/"+e+"/called"]);window["to"+"ken"]="synthetic-telemetry-canary";})()})();"#;

#[test]
fn known_wiaa_telemetry_is_removed_while_public_dom_and_athletic_json_survive() -> Result<()> {
    let public = include_str!("../../../fixtures/contacts/wiaa-abbotsford-detail.html");
    let json = r#"<script type="application/ld+json">{"name":"Public Athlete","event":"800m","best":"2:10.67"}</script>"#;
    let link = "<a href='/Directory/School/GetDirectorySchool?orgID=320'>Public directory</a>";
    let expected = format!("<script></script>{public}{json}{link}");
    for version in ["1.324.0", "1.324.1"] {
        let prefix = PREFIX.replace("1.324.0", version);
        let source = format!("<script>{prefix}{LOADER}</script>{public}{json}{link}");
        let retained = body(source.as_bytes())?;
        let text = std::str::from_utf8(&retained)?;
        assert_eq!(text, expected);
        assert_eq!(public_dom(text), public_dom(&expected));
        assert_eq!(public_dom(text).1.len(), 9);
        assert_eq!(
            public_dom(text).1.last().map(String::as_str),
            Some("/Directory/School/GetDirectorySchool?orgID=320")
        );
        assert!(!text.contains("NREUM"));
        assert!(!text.contains("synthetic-telemetry-canary"));
        assert!(!text.contains("synthetic-public-id"));
        assert_eq!(body(&retained)?, retained);
    }
    Ok(())
}

fn public_dom(text: &str) -> (String, Vec<String>) {
    let document = scraper::Html::parse_document(text);
    let anchors = scraper::Selector::parse("a[href]").expect("constant anchor selector");
    (
        document.root_element().text().collect::<String>(),
        document
            .select(&anchors)
            .map(|anchor| {
                anchor
                    .value()
                    .attr("href")
                    .expect("selected href")
                    .to_owned()
            })
            .collect(),
    )
}

#[test]
fn telemetry_marker_never_authorizes_retention_of_unknown_computed_scripts() {
    for script in [
        LOADER.to_owned(),
        format!("{PREFIX}window['to'+'ken']='synthetic-unknown-canary';"),
        format!("{PREFIX}{LOADER}").replace("1.324.0", "1.325.0"),
        format!("{PREFIX}window['to'+'ken']='synthetic-unknown-canary';")
            .replace("1.324.0", "1.324.1"),
        format!("{PREFIX}{LOADER}").replace("(window.NREUM||(NREUM={})).init=", "other.init="),
        format!("{PREFIX}{LOADER}")
            .replace("1.324.0", "1.324.1")
            .replace("(window.NREUM||(NREUM={})).init=", "other.init="),
    ] {
        let failure = body(format!("<script>{script}</script>").as_bytes())
            .expect_err("unrecognized computed script must remain fail closed");
        assert!(!format!("{failure:#}").contains("canary"));
    }
    let source = format!(
        "<script>{PREFIX}{LOADER}</script><script>window['to'+'ken']='synthetic-neighbor-canary';</script>"
    );
    assert!(body(source.as_bytes()).is_err());
    assert!(body(source.replace("1.324.0", "1.324.1").as_bytes()).is_err());
}

#[test]
fn reviewed_telemetry_version_alone_does_not_authorize_an_unknown_envelope() {
    for version in ["1.324.0", "1.324.1"] {
        let script = format!("{PREFIX}{LOADER}")
            .replace("1.324.0", version)
            .replace("(window.NREUM||(NREUM={})).init=", "other.init=");
        let diagnostic = format!(
            "{:#}",
            body(format!("<script>{script}</script>").as_bytes())
                .expect_err("reviewed version still requires the known envelope")
        );
        assert!(
            diagnostic.contains(
                "nreum_prefix=false;loader_config=true;license=true;pinned_license=true;"
            )
        );
        assert!(diagnostic.contains(&format!("vendor_version={version}")));
        assert!(!diagnostic.contains("synthetic"));
    }
}

#[test]
fn telemetry_recognition_cannot_cross_native_html_script_boundaries() {
    for injected in [
        r#"const value="</script><script>window['to'+'ken']='synthetic-boundary-canary';</script>";"#,
        r#"const value="</SCRIPT><p>window['to'+'ken']='synthetic-boundary-canary';</p>";"#,
    ] {
        let source = format!("<script>{PREFIX}(()=>{{{injected}{LOADER}</script>");
        assert!(body(source.as_bytes()).is_err());
    }
    for source in [
        format!("<!-- <script>{PREFIX}{LOADER}</script> -->"),
        format!("<p data-script='<script>{PREFIX}{LOADER}</script>'>public</p>"),
    ] {
        assert!(body(source.as_bytes()).is_err());
    }
}

#[test]
fn rejected_script_diagnostics_are_bounded_and_never_include_script_values() {
    use sha2::{Digest, Sha256};

    let private = "synthetic-private-canary".repeat(100_000);
    let script = format!("window['se'+'cret']='{private}';grecaptcha.execute();");
    let error = body(format!("<script>{script}</script>").as_bytes())
        .expect_err("unrecognized computed script must still reject");
    let diagnostic = format!("{error:#}");
    assert!(diagnostic.starts_with(
        "scanning source script boundaries: cannot safely classify computed script assignments ["
    ));
    assert!(diagnostic.len() <= 384);
    assert!(diagnostic.contains(&format!("script_bytes={}", script.len())));
    assert!(diagnostic.contains(&format!("sha256={:x}", Sha256::digest(script.as_bytes()))));
    assert!(diagnostic.contains("nreum_prefix=false;loader_config=false;license=false;pinned_license=false;closure=false;challenge_marker=true;vendor_version=unknown"));
    assert!(!diagnostic.contains("synthetic"));
    assert!(!diagnostic.contains("grecaptcha"));
    assert!(!diagnostic.contains("window"));
}

#[test]
fn diagnostic_vendor_version_is_strict_numeric_metadata_and_does_not_authorize_removal() {
    for version in [
        "1.325.0",
        "123456.123456.123456",
        "1.2.synthetic-private-canary",
        "1.2.3-private",
        "1..3",
        "1.2.3.4",
        "1234567.2.3",
        "\u{ff11}.2.3",
    ] {
        let script = format!("{PREFIX}{LOADER}").replace("1.324.0", version);
        let error = body(format!("<script>{script}</script>").as_bytes())
            .expect_err("new or malformed loader remains unrecognized");
        let diagnostic = format!("{error:#}");
        let numeric = matches!(version, "1.325.0" | "123456.123456.123456");
        assert!(diagnostic.len() <= 384);
        assert!(diagnostic.contains("nreum_prefix=true;loader_config=true;"));
        assert!(diagnostic.contains("pinned_license=false;closure=true;challenge_marker=false;"));
        assert!(diagnostic.contains(&format!("license={numeric};")));
        assert!(diagnostic.contains(&format!(
            "vendor_version={}",
            if numeric { version } else { "unknown" }
        )));
        assert!(!diagnostic.contains("synthetic-private-canary"));
        assert!(!diagnostic.contains("synthetic-public-id"));
        assert!(!diagnostic.contains("synthetic-telemetry-canary"));
    }
}
