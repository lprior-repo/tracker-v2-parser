use super::*;
use crate::{
    database::Job,
    durable::page::{self, PageScope},
    model::{PageContext, Source, Target},
};

const FAILURE: &str =
    "scanning source script boundaries: cannot safely classify computed script assignments";

fn config(review: &str) -> Result<Config> {
    Config::parse(&format!(
        "states=['WI']\n[acquisition]\nmode='public_results'\ncollect_contacts=true\n{review}"
    ))
}

#[test]
fn reviewed_legacy_configuration_is_explicit_bounded_and_old_digest_compatible() -> Result<()> {
    let default = config("")?;
    let empty = config("reviewed_legacy_failures=[]")?;
    let old = serde_json::json!({"mode":"public_results","reviewed_media_stops":[],"collect_contacts":true,"profile_coverage_only":false});
    assert_eq!(serde_json::to_value(&default.acquisition)?, old);
    assert_eq!(
        crate::durable::model::page_config_digest(&default)?,
        crate::durable::model::page_config_digest(&empty)?
    );
    let reviewed = config("reviewed_legacy_failures=['wiaa_script_sanitization']")?;
    assert_ne!(
        crate::durable::model::page_config_digest(&default)?,
        crate::durable::model::page_config_digest(&reviewed)?
    );
    for review in [
        "reviewed_legacy_failures=['all']",
        "reviewed_legacy_failures=['wiaa_script_sanitization','wiaa_script_sanitization']",
    ] {
        assert!(config(review).is_err());
    }
    assert!(Config::parse("[acquisition]\nmode='public_results'\nreviewed_legacy_failures=['wiaa_script_sanitization']").is_err());
    Ok(())
}

#[test]
fn reviewed_legacy_permit_is_confined_to_official_contacts_and_exact_technical_failure()
-> Result<()> {
    let config = config("reviewed_legacy_failures=['wiaa_script_sanitization']")?;
    let scope = PageScope {
        run_generation: "reviewed-wiaa".into(),
        config_sha256: crate::durable::model::page_config_digest(&config)?,
        deadline_ms: 1_800_000_000_000,
        cooldown_ms: 15_000,
    };
    let target = Target::get(
        Source::MileSplit,
        PageKind::Contacts,
        "https://schools.wiaawi.org/Directory/School/List".into(),
        PageContext::default(),
    );
    let mut request = page::request(Job { id: 1, target }, &scope)?;
    let permit = LegacyFailurePermit::bind(&config, &request)?
        .ok_or_else(|| anyhow::anyhow!("review permit missing"))?;
    for url in [
        "https://schools.wiaawi.org/Directory/School/List",
        "https://schools.wiaawi.org/Directory/School/DirectoryLetter?LetterBtn=A",
        "https://schools.wiaawi.org/Directory/School/GetDirectorySchool?orgID=123",
    ] {
        request.target.url = url.into();
        assert!(permit.permits(&request, FAILURE), "{url}");
    }
    for url in [
        "http://schools.wiaawi.org/Directory/School/List",
        "https://evil.example/Directory/School/List",
        "https://schools.wiaawi.org.evil.example/Directory/School/List",
        "https://schools.wiaawi.org/Directory/School/List?x=1",
        "https://schools.wiaawi.org/Directory/School/List#fragment",
        "https://schools.wiaawi.org/Directory/School/DirectoryLetter?LetterBtn=ab",
        "https://schools.wiaawi.org/Directory/School/GetDirectorySchool?orgID=0",
        "https://user@schools.wiaawi.org/Directory/School/List",
        "https://schools.wiaawi.org/Directory/School/%4cist",
    ] {
        request.target.url = url.into();
        assert!(!permit.permits(&request, FAILURE), "{url}");
    }
    request.target.url = "https://schools.wiaawi.org/Directory/School/List".into();
    for detail in [
        "HTTP 403",
        "login required",
        "CAPTCHA",
        "invalid required identity",
        "scanning source script boundaries: other sanitization error",
    ] {
        assert!(!permit.permits(&request, detail));
    }
    for marker in [
        "HTTP 403",
        "login required",
        "CAPTCHA",
        "access stop",
        "challenge_marker=true",
    ] {
        assert!(!permit.permits(&request, &format!("{FAILURE} [{marker}]")));
    }
    assert!(permit.permits(
        &request,
        &format!("{FAILURE} [challenge_marker=false;sha256=403]")
    ));
    request.target.kind = PageKind::Athlete;
    assert!(!permit.permits(&request, FAILURE));
    request.binding.scope.config_sha256 = "0".repeat(64);
    assert!(!permit.permits(&request, FAILURE));
    assert!(LegacyFailurePermit::bind(&config, &request).is_err());
    Ok(())
}
