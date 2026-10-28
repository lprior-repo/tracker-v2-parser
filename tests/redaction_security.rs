use anyhow::Result;
use track_scrapper_v2::{model::Capture, transport::redact_capture};

fn capture(body: &[u8]) -> Capture {
    Capture {
        url: "https://example.org/results".to_owned(),
        status: 200,
        content_type: "application/json".to_owned(),
        body: body.to_vec(),
    }
}

#[test]
fn escaped_keys_and_all_json_value_types_are_redacted_without_losing_source_data() -> Result<()> {
    let source = br#"{"to\u006ben":123,"nested":[{"AUTHORIZ\u0041TION":{"private":"secret"}},{"cookie":["a",42]},{"jwt":true}],"Result":"44.88","duplicate":1,"duplicate":2,"large":123456789012345678901234567890}"#;
    let captured = redact_capture(capture(source))?;
    let text = std::str::from_utf8(&captured.body)?;
    assert!(!text.contains("secret"));
    assert!(!text.contains("[\"a\",42]"));
    assert!(text.contains("\"duplicate\":1,\"duplicate\":2"));
    assert!(text.contains("123456789012345678901234567890"));
    let value: serde_json::Value = serde_json::from_slice(&captured.body)?;
    assert_eq!(value["token"], "[REDACTED]");
    assert_eq!(value["nested"][0]["AUTHORIZATION"], "[REDACTED]");
    assert_eq!(value["nested"][1]["cookie"], "[REDACTED]");
    assert_eq!(value["nested"][2]["jwt"], "[REDACTED]");
    assert_eq!(value["Result"], "44.88");
    Ok(())
}

#[test]
fn escaped_jwt_in_public_string_is_redacted() -> Result<()> {
    let captured = redact_capture(capture(
        br#"{"Result":"44.88","detail":"ey\u004ahbGciOiJIUzI1NiJ9.eyJ0ZWFtIjozMjA0fQ.signature"}"#,
    ))?;
    let value: serde_json::Value = serde_json::from_slice(&captured.body)?;
    assert_eq!(value["detail"], "[REDACTED]");
    assert_eq!(value["Result"], "44.88");
    Ok(())
}

#[test]
fn malformed_json_and_ambiguous_html_never_produce_a_capture() {
    assert!(redact_capture(capture(br#"{"to\u006ben":"secret""#)).is_err());
    assert!(redact_capture(capture(b"<p>&#116;oken=secret</p>")).is_err());
    assert!(redact_capture(capture(&[0xff, 0xfe])).is_err());
}

#[test]
fn html_assignments_are_redacted_without_losing_public_content() -> Result<()> {
    let source = br#"<p>Cookie Preferences &amp; Men&#39;s</p><script>const segmentResetToken = null; const userToken = 'private'; const data = {"to\u006ben":{"credential":"private"},"twitterSecret":"private"};</script>"#;
    let captured = redact_capture(capture(source))?;
    let text = std::str::from_utf8(&captured.body)?;
    assert!(text.contains("Cookie Preferences &amp; Men&#39;s"));
    assert!(!text.contains("private"));
    assert!(text.contains("segmentResetToken = \"[REDACTED]\";"));
    assert!(text.contains("userToken = \"[REDACTED]\""));
    assert!(text.contains("\"twitterSecret\":\"[REDACTED]\""));
    Ok(())
}

#[test]
fn captured_milesplit_page_retains_results_and_removes_script_secrets() -> Result<()> {
    let source = include_bytes!("../fixtures/milesplit/results-oh.html");
    let captured = redact_capture(capture(source))?;
    let text = std::str::from_utf8(&captured.body)?;
    assert!(text.contains("Ahmann&#039;s Park"));
    assert!(text.contains("Cookie Preferences"));
    assert!(text.contains("\"twitterToken\":\"[REDACTED]\""));
    assert!(text.contains("\"twitterSecret\":\"[REDACTED]\""));
    Ok(())
}

#[test]
fn staff_page_with_dynamic_cookie_assignment_retains_only_safe_public_coaching_claims() -> Result<()>
{
    use track_scrapper_v2::model::{PageContext, PageKind, Source, Target};
    let source = include_bytes!("../fixtures/contacts/miramonte-staff.html");
    let retained = redact_capture(capture(source))?;
    let target = Target::get(
        Source::AthleticNet,
        PageKind::Contacts,
        "https://gomats.org/staff-directory".into(),
        PageContext {
            school_id: Some("school-123".into()),
            school_name: Some("Miramonte High School".into()),
            state: Some("CA".into()),
            season: Some(2026),
            ..PageContext::default()
        },
    );
    let page = track_scrapper_v2::sources::parse(&target, std::str::from_utf8(&retained.body)?)?;
    assert_eq!(page.coaches.len(), 3);
    assert!(
        page.coaches
            .iter()
            .any(|coach| coach.name.as_deref() == Some("Brian Henderson")
                && coach.email.as_deref() == Some("bhenderson@auhsdschools.org")
                && coach.role == "Head Coach — Cross Country")
    );
    Ok(())
}

#[test]
fn ordinary_html_is_preserved() -> Result<()> {
    let source = b"<table><tr><td>Athlete</td><td>44.88</td></tr></table>";
    assert_eq!(redact_capture(capture(source))?.body, source);
    Ok(())
}

#[test]
fn given_braced_unicode_secret_identifier_when_retained_then_canary_is_removed() -> Result<()> {
    for assignment in [
        r"window.to\u{6b}en = 'synthetic-braced-canary';",
        r"window.\u{74}oken = 'synthetic-braced-canary';",
        r"window.to\u{00006B}en = 'synthetic-braced-canary';",
    ] {
        let source = format!(
            "<p>Public Athlete 44.88</p><script>{assignment}</script><script type='application/ld+json'>{{\"name\":\"Public Athlete\"}}</script>"
        );
        let retained = redact_capture(capture(source.as_bytes()))?;
        let text = std::str::from_utf8(&retained.body)?;
        assert!(!text.contains("synthetic-braced-canary"), "{assignment}");
        assert!(text.contains("<p>Public Athlete 44.88</p>"));
        assert!(text.contains("{\"name\":\"Public Athlete\"}"));
        assert_eq!(redact_capture(capture(&retained.body))?.body, retained.body);
    }
    Ok(())
}

#[test]
fn given_harmless_sdk_computed_reads_and_public_json_when_retained_then_source_is_preserved()
-> Result<()> {
    let source = br#"<main><p>Public Athlete 44.88</p><a href="mailto:coach@example.org">Public Coach</a></main><script>sdk[(index)] = handler; sdk[(handlers[index])](event); const name = sdk[(`event`)]; const flags = [`enabled`]; const grouped = sdk[(("event"))] === "ready";</script><script type="application/ld+json">{"name":"Public Athlete","Result":"44.88","coach":"Public Coach"}</script>"#;
    assert_eq!(redact_capture(capture(source))?.body, source);
    Ok(())
}
