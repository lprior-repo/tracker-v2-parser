#[cfg(test)]
mod tests;

use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use anyhow::{Context, Result};
use reqwest::Client;
use tokio::time::Instant;
use url::Url;

const MAX_CACHED_ORIGINS: usize = 16;
const MAX_CACHED_ADDRESSES: usize = 32;
const MAX_ORIGIN_BYTES: usize = 2048;
const CLIENT_TTL: Duration = Duration::from_mins(1);

#[derive(Clone, Default)]
pub struct SharedHttpClients {
    state: Arc<Mutex<BTreeMap<String, CachedClient>>>,
}

struct CachedClient {
    addresses: Vec<SocketAddr>,
    client: Client,
    created: Instant,
}

impl SharedHttpClients {
    fn state(&self) -> Result<MutexGuard<'_, BTreeMap<String, CachedClient>>> {
        self.state
            .lock()
            .map_err(|error| anyhow::anyhow!("HTTP client cache poisoned: {error}"))
    }

    pub(super) fn client(&self, url: &Url, addresses: &[SocketAddr]) -> Result<Client> {
        super::policy::validate_public_addresses(addresses)?;
        let origin = url.origin().ascii_serialization();
        if addresses.len() > MAX_CACHED_ADDRESSES || origin.len() > MAX_ORIGIN_BYTES {
            return build_client(url, addresses, true);
        }
        let mut addresses = addresses.to_vec();
        addresses.sort_unstable();
        addresses.dedup();
        let now = Instant::now();
        if let Some(client) = self.lookup(&origin, &addresses, now)? {
            return Ok(client);
        }
        let client = build_client(url, &addresses, true)?;
        let mut state = self.state()?;
        state.retain(|_, entry| now.saturating_duration_since(entry.created) < CLIENT_TTL);
        if state.len() >= MAX_CACHED_ORIGINS && !state.contains_key(&origin) {
            let oldest = state
                .iter()
                .min_by_key(|(_, entry)| entry.created)
                .map(|(origin, _)| origin.clone());
            if let Some(oldest) = oldest {
                state.remove(&oldest);
            }
        }
        state.insert(
            origin,
            CachedClient {
                addresses,
                client: client.clone(),
                created: now,
            },
        );
        drop(state);
        Ok(client)
    }

    fn lookup(
        &self,
        origin: &str,
        addresses: &[SocketAddr],
        now: Instant,
    ) -> Result<Option<Client>> {
        Ok(self
            .state()?
            .get(origin)
            .filter(|entry| {
                entry.addresses == addresses
                    && now.saturating_duration_since(entry.created) < CLIENT_TTL
            })
            .map(|entry| entry.client.clone()))
    }
}

pub(super) fn build_client(
    url: &Url,
    addresses: &[SocketAddr],
    persistent: bool,
) -> Result<Client> {
    let host = url.host_str().context("source URL has no host")?;
    let builder = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(45))
        .connect_timeout(Duration::from_secs(15))
        .pool_idle_timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(2)
        .user_agent("track-scrapper-v2/0.1 public athletics research")
        .resolve_to_addrs(host, addresses);
    let builder = if persistent {
        builder.retry(reqwest::retry::never())
    } else {
        builder
    };
    builder
        .build()
        .context("building pinned public HTTP client")
}
