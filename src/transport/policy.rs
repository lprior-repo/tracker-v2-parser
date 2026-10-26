use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use anyhow::{Context, Result, ensure};
use url::Url;

use crate::model::{PageKind, Source, Target};

pub(super) fn source_url(target: &Target, address: &str) -> Result<Url> {
    let url = Url::parse(address).context("parsing source URL")?;
    ensure!(
        matches!(url.scheme(), "https" | "http"),
        "source URL must use HTTPS or HTTP"
    );
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "source URL must not contain credentials"
    );
    ensure!(
        matches!(url.port_or_known_default(), Some(80 | 443)),
        "source URL uses an unsupported port"
    );
    let host = url.host_str().context("source URL has no host")?;
    if target.kind != PageKind::Contacts {
        let domain = match target.source {
            Source::AthleticNet => "athletic.net",
            Source::MileSplit => "milesplit.com",
        };
        ensure!(
            host == domain || host.ends_with(&format!(".{domain}")),
            "source URL is outside the selected website"
        );
    }
    ensure!(
        !url.query_pairs().any(|(name, _)| {
            let name = name.to_ascii_lowercase();
            ["token", "password", "authorization", "cookie"]
                .iter()
                .any(|secret| name.contains(secret))
        }),
        "source URL must not carry authentication secrets"
    );
    Ok(url)
}

pub(super) async fn public_addresses(url: &Url) -> Result<Vec<SocketAddr>> {
    let host = url.host_str().context("source URL has no host")?;
    let port = url
        .port_or_known_default()
        .context("source URL has no port")?;
    let addresses = tokio::net::lookup_host((host, port))
        .await
        .context("resolving source host")?
        .collect::<Vec<_>>();
    ensure!(
        !addresses.is_empty() && addresses.iter().all(|address| public_ip(address.ip())),
        "source resolved to a private, local, or reserved address"
    );
    Ok(addresses)
}

fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => public_v4(ip),
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return public_v4(mapped);
            }
            let [first, second, ..] = ip.segments();
            first & 0xe000 == 0x2000 && !(first == 0x2001 && second == 0x0db8)
        }
    }
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_broadcast()
        || ip.is_documentation()
        || a == 0
        || a >= 240
        || (a == 100 && (64..=127).contains(&b))
        || (a == 198 && (18..=19).contains(&b))
        || (a == 192 && b == 0 && c == 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_private_mapped_and_reserved_source_addresses() -> Result<()> {
        [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.169.254",
            "100.64.1.1",
            "::1",
            "::ffff:127.0.0.1",
            "2001:db8::1",
        ]
        .iter()
        .try_for_each(|text| -> Result<()> {
            assert!(!public_ip(text.parse()?));
            Ok(())
        })?;
        assert!(public_ip("8.8.8.8".parse()?));
        assert!(public_ip("192.0.78.13".parse()?));
        Ok(())
    }
}
