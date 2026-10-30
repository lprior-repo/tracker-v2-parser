use super::sanitize_response;
use crate::model::Capture;

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
