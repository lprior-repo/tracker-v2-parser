use super::{Mode, Options, Transport};
use crate::model::{PageKind, Source, Target};
use anyhow::Result;
use std::time::Duration;
use tokio::time::Instant;

fn transport(rps: f64) -> Result<Transport> {
    Transport::new(Options {
        mode: Mode::Http,
        driver_url: None,
        profile: Default::default(),
        headless: true,
        rps,
    })
}

fn contact(path: &str) -> Target {
    Target::get(
        Source::MileSplit,
        PageKind::Contacts,
        format!("https://school.example.edu/{path}"),
        Default::default(),
    )
}

#[tokio::test(start_paused = true)]
async fn explicit_contact_rate_paces_both_regular_and_staff_directory_targets() -> Result<()> {
    for path in ["contact", "staff-directory"] {
        let target = contact(path);
        let mut transport = transport(3.0)?.with_contact_rate(Some(3.0))?;
        let expected = Duration::from_nanos(333_333_333);
        assert_eq!(transport.request_interval(&target)?, expected);
        transport.pace(&target).await?;
        let start = Instant::now();
        transport.pace(&target).await?;
        assert!(start.elapsed() >= expected);
        assert!(start.elapsed() <= expected + Duration::from_millis(1));
        assert_eq!(
            transport.next_request.get(&target.source),
            Some(&(Instant::now() + expected))
        );
    }
    Ok(())
}

#[test]
fn contact_override_is_bounded_by_global_rate_and_preserves_default_caps() -> Result<()> {
    for path in ["contact", "staff-directory"] {
        let target = contact(path);
        let limited = transport(2.0)?.with_contact_rate(Some(3.0))?;
        assert_eq!(
            limited.request_interval(&target)?,
            Duration::from_millis(500)
        );
        let slower = transport(3.0)?.with_contact_rate(Some(1.0))?;
        assert_eq!(slower.request_interval(&target)?, Duration::from_secs(1));
        let default = transport(3.0)?.with_contact_rate(None)?;
        assert_eq!(
            default.request_interval(&target)?,
            Duration::from_secs(if path == "staff-directory" { 30 } else { 5 })
        );
    }
    let mut athlete = contact("staff-directory");
    athlete.kind = PageKind::Athlete;
    let ordinary = transport(3.0)?.with_contact_rate(Some(0.2))?;
    assert_eq!(
        ordinary.request_interval(&athlete)?,
        Duration::from_nanos(333_333_333)
    );
    for rate in [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        0.0,
        -1.0,
        3.0001,
    ] {
        assert!(transport(3.0)?.with_contact_rate(Some(rate)).is_err());
    }
    Ok(())
}
