use anyhow::{Context, Result};
use url::Url;

use super::AccessStop;

#[derive(Clone, Copy)]
enum Escape {
    Plain,
    First,
    Second(u8),
}

pub(crate) fn check(address: &str) -> Result<()> {
    let url = Url::parse(address).context("parsing robots policy destination")?;
    let Some(host) = url.host_str() else {
        return Ok(());
    };
    if host != "milesplit.com" && !host.ends_with(".milesplit.com") {
        return Ok(());
    }
    let path = decoded_path(url.path()).map_err(|error| {
        stop(&url).context(format!("ambiguous MileSplit robots policy path: {error:#}"))
    })?;
    let path = path.trim_start_matches('/').to_ascii_lowercase();
    if ["rankings", "virtual-meets", "api/", "contact"]
        .iter()
        .any(|prefix| path.starts_with(prefix))
    {
        return Err(stop(&url).context(
            "MileSplit published robots restrictions prohibit this route; subdomains remain conservatively restricted pending host-specific policy review",
        ));
    }
    Ok(())
}

fn stop(url: &Url) -> anyhow::Error {
    anyhow::Error::new(AccessStop {
        origin: url.origin().ascii_serialization(),
        status: None,
    })
}

fn decoded_path(path: &str) -> Result<String> {
    anyhow::ensure!(path.len() <= 8192, "policy path exceeds 8192 bytes");
    let (bytes, state) = path.bytes().try_fold(
        (Vec::with_capacity(path.len()), Escape::Plain),
        |(mut bytes, state), byte| -> Result<_> {
            let next = match (state, byte) {
                (Escape::Plain, b'%') => Escape::First,
                (Escape::Plain, value) => {
                    bytes.push(value);
                    Escape::Plain
                }
                (Escape::First, value) => Escape::Second(hex(value)?),
                (Escape::Second(high), value) => {
                    let low = hex(value)?;
                    bytes.push(
                        high.checked_mul(16)
                            .and_then(|high| high.checked_add(low))
                            .context("path escape overflow")?,
                    );
                    Escape::Plain
                }
            };
            Ok((bytes, next))
        },
    )?;
    anyhow::ensure!(matches!(state, Escape::Plain), "truncated path escape");
    anyhow::ensure!(!bytes.contains(&b'%'), "nested path escape");
    String::from_utf8(bytes).context("non-UTF8 policy path")
}

fn hex(byte: u8) -> Result<u8> {
    let value = char::from(byte)
        .to_digit(16)
        .context("invalid path escape")?;
    u8::try_from(value).context("path escape overflow")
}
