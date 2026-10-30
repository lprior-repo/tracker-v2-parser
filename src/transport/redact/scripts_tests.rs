use anyhow::Result;

use super::super::body;

const PREFIX: &str = "(window.NREUM||(NREUM={})).init={privacy:{cookies_enabled:true}};(window.NREUM||(NREUM={})).loader_config={agentID:'synthetic-public-id'};;/*! For license information please see nr-loader-spa-1.324.0.min.js.LICENSE.txt */";
const LOADER: &str = r#"(()=>{(()=>{record(["API/"+e+"/called"]);window["to"+"ken"]="synthetic-telemetry-canary";})()})();"#;

#[test]
fn known_wiaa_telemetry_is_removed_while_public_dom_and_athletic_json_survive() -> Result<()> {
    let public = include_str!("../../../fixtures/contacts/wiaa-abbotsford-detail.html");
    let json = r#"<script type="application/ld+json">{"name":"Public Athlete","event":"800m","best":"2:10.67"}</script>"#;
    let source = format!("<script>{PREFIX}{LOADER}</script>{public}{json}");
    let retained = body(source.as_bytes())?;
    let text = std::str::from_utf8(&retained)?;
    assert_eq!(text, format!("<script></script>{public}{json}"));
    assert!(!text.contains("NREUM"));
    assert!(!text.contains("synthetic-telemetry-canary"));
    assert!(!text.contains("synthetic-public-id"));
    assert_eq!(body(&retained)?, retained);
    Ok(())
}

#[test]
fn telemetry_marker_never_authorizes_retention_of_unknown_computed_scripts() {
    for script in [
        LOADER.to_owned(),
        format!("{PREFIX}window['to'+'ken']='synthetic-unknown-canary';"),
        format!("{PREFIX}{LOADER}").replace("1.324.0", "1.324.1"),
        format!("{PREFIX}{LOADER}").replace("(window.NREUM||(NREUM={})).init=", "other.init="),
    ] {
        let failure = body(format!("<script>{script}</script>").as_bytes())
            .expect_err("unrecognized computed script must remain fail closed");
        assert!(!format!("{failure:#}").contains("canary"));
    }
    let source = format!(
        "<script>{PREFIX}{LOADER}</script><script>window['to'+'ken']='synthetic-neighbor-canary';</script>"
    );
    assert!(body(source.as_bytes()).is_err());
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
