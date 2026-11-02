use std::time::{Duration, Instant};

use super::{Failure, redact_capture, sanitize_response};
use crate::model::Capture;

fn capture(status: u16, content_type: &str, body: &[u8]) -> Capture {
    Capture {
        url: "https://school.example.edu/profile".into(),
        status,
        content_type: content_type.into(),
        body: body.to_vec(),
    }
}

fn assert_capture_eq(left: &Capture, right: &Capture) {
    assert_eq!(left.url, right.url);
    assert_eq!(left.status, right.status);
    assert_eq!(left.content_type, right.content_type);
    assert_eq!(left.body, right.body);
}

#[test]
fn sanitizer_diagnostics_do_not_override_observed_access_classification() {
    let ordinary = "<main>Public school</main><script>window['api'+'Key']='private-sanitizer-canary';</script>";
    let challenged = format!("<title>Verify you are human</title>{ordinary}");
    let script_challenge =
        format!("{ordinary}<script src='/cdn-cgi/challenge-platform/cf-chl-script.js'></script>");
    let true_marker = format!(
        "<title>Verify you are human</title><script>window['api'+'Key']='private-sanitizer-canary';grecaptcha.execute();</script>"
    );
    for (status, path, body, blocked, marker) in [
        (200, "/staff", ordinary, false, false),
        (200, "/staff", challenged.as_str(), true, false),
        (200, "/staff", script_challenge.as_str(), true, false),
        (200, "/staff", true_marker.as_str(), true, true),
        (403, "/staff", ordinary, true, false),
        (200, "/login", ordinary, true, false),
    ] {
        let failure = sanitize_response(Capture {
            url: format!("https://school.example.edu{path}"),
            status,
            content_type: "text/html".into(),
            body: body.as_bytes().to_vec(),
        })
        .expect_err("unsupported script must still reject");
        assert_eq!(failure.blocked, blocked);
        assert!(!failure.retryable);
        assert!(failure.capture.is_none());
        assert!(
            failure
                .detail
                .contains(&format!("challenge_marker={marker}"))
        );
        assert!(!format!("{failure:?}").contains("private-sanitizer-canary"));
        assert!(!format!("{failure:?}").contains("window"));
    }
}

#[test]
fn sanitizing_then_redacting_preserves_exact_capture_bytes_and_metadata() -> anyhow::Result<()> {
    let cases = [
        (
            "text/html",
            b"<main>Public profile</main><script>const apiKey='html-secret-canary';</script>"
                .as_slice(),
        ),
        (
            "application/json",
            br#"{"name":"Public profile","apiKey":"json-secret-canary"}"#.as_slice(),
        ),
    ];
    for (content_type, body) in cases {
        let once = sanitize_response(capture(200, content_type, body))
            .expect("representative response sanitizes");
        let old_path = redact_capture(sanitize_response(capture(200, content_type, body))?)
            .expect("second redaction preserves valid sanitized response");
        assert_capture_eq(&once, &old_path);
        let output = String::from_utf8(once.body.clone()).expect("sanitized response is UTF-8");
        assert!(!output.contains("html-secret-canary"));
        assert!(!output.contains("json-secret-canary"));
    }
    Ok(())
}

#[test]
fn malformed_and_access_stop_responses_keep_their_failure_classification() {
    let malformed = sanitize_response(capture(200, "application/json", br#"{"apiKey":"secret""#))
        .expect_err("malformed JSON must remain rejected");
    assert_failure_class(&malformed, false);

    let access_stop = sanitize_response(capture(403, "application/json", br#"{"apiKey":"secret""#))
        .expect_err("access stop must remain blocked");
    assert_failure_class(&access_stop, true);
}

fn assert_failure_class(failure: &Failure, blocked: bool) {
    assert_eq!(failure.blocked, blocked);
    assert!(!failure.retryable);
    assert!(failure.capture.is_none());
}

#[test]
#[ignore = "bounded local comparison; run explicitly with --ignored --nocapture"]
fn benchmark_duplicate_redaction_for_bounded_html_and_json_fixtures() -> anyhow::Result<()> {
    const SIZES: [usize; 4] = [256, 65_536, 4_194_304, 16_777_216];
    const SAMPLES: usize = 20;
    for size in SIZES {
        for (label, content_type, body) in [
            ("html", "text/html", bounded_html(size)),
            ("json", "application/json", bounded_json(size)),
        ] {
            let mut single = Vec::with_capacity(SAMPLES);
            let mut old = Vec::with_capacity(SAMPLES);
            for sample_index in 0..SAMPLES {
                let (single_elapsed, single_capture, old_elapsed, old_capture) =
                    if sample_index % 2 == 0 {
                        let (elapsed, capture) = timed_single(content_type, body.as_bytes())?;
                        let (old_elapsed, old_capture) = timed_old(content_type, body.as_bytes())?;
                        (elapsed, capture, old_elapsed, old_capture)
                    } else {
                        let (old_elapsed, old_capture) = timed_old(content_type, body.as_bytes())?;
                        let (elapsed, capture) = timed_single(content_type, body.as_bytes())?;
                        (elapsed, capture, old_elapsed, old_capture)
                    };
                assert_capture_eq(&single_capture, &old_capture);
                single.push(single_elapsed);
                old.push(old_elapsed);
            }
            report_samples(size, label, &mut single, &mut old);
        }
    }
    Ok(())
}

fn bounded_html(size: usize) -> String {
    const SUFFIX: &str = "</main><script>const apiKey='fixture-secret';</script>";
    let prefix = "<main>";
    let fill = size - prefix.len() - SUFFIX.len();
    format!("{prefix}{}{SUFFIX}", "x".repeat(fill))
}

fn bounded_json(size: usize) -> String {
    const PREFIX: &str = "{\"payload\":\"";
    const SUFFIX: &str = "\",\"apiKey\":\"fixture-secret\"}";
    let fill = size - PREFIX.len() - SUFFIX.len();
    format!("{PREFIX}{}{SUFFIX}", "x".repeat(fill))
}

fn timed_single(content_type: &str, body: &[u8]) -> anyhow::Result<(Duration, Capture)> {
    let start = Instant::now();
    let sanitized = sanitize_response(capture(200, content_type, body))?;
    let elapsed = start.elapsed();
    Ok((elapsed, std::hint::black_box(sanitized)))
}

fn timed_old(content_type: &str, body: &[u8]) -> anyhow::Result<(Duration, Capture)> {
    let start = Instant::now();
    let sanitized = sanitize_response(capture(200, content_type, body))?;
    let redacted = redact_capture(sanitized)?;
    let elapsed = start.elapsed();
    Ok((elapsed, std::hint::black_box(redacted)))
}

fn report_samples(bytes: usize, format: &str, single: &mut [Duration], old: &mut [Duration]) {
    single.sort_unstable();
    old.sort_unstable();
    let p50 = single[single.len() / 2];
    let p95 = single[(single.len() * 95).div_ceil(100) - 1];
    let max = *single.last().expect("nonempty single samples");
    let old_p50 = old[old.len() / 2];
    let old_p95 = old[(old.len() * 95).div_ceil(100) - 1];
    let old_max = *old.last().expect("nonempty old samples");
    eprintln!(
        "capture_redaction format={format} bytes={bytes} samples={} single_p50_us={} single_p95_us={} single_max_us={} old_p50_us={} old_p95_us={} old_max_us={}",
        single.len(),
        p50.as_micros(),
        p95.as_micros(),
        max.as_micros(),
        old_p50.as_micros(),
        old_p95.as_micros(),
        old_max.as_micros()
    );
}
