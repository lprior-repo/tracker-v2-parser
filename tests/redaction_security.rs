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
fn staff_page_preserves_contact_data_and_redacts_cookie_assignments() -> Result<()> {
    let source = include_bytes!("../fixtures/contacts/miramonte-staff.html");
    let captured = redact_capture(capture(source))?;
    let text = std::str::from_utf8(&captured.body)?;
    assert!(text.contains("auhsdschools.org"));
    assert!(text.contains("Men&#39;s Water Polo"));
    assert!(text.contains("CookieDate = \"[REDACTED]\";"));
    assert!(text.contains("document.cookie = \"[REDACTED]\""));
    Ok(())
}

#[test]
fn ordinary_html_is_preserved() -> Result<()> {
    let source = b"<table><tr><td>Athlete</td><td>44.88</td></tr></table>";
    assert_eq!(redact_capture(capture(source))?.body, source);
    Ok(())
}
