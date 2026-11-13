use serde_json::Value;
use url::Url;

use crate::{
    model::{PageKind, Source, Target},
    scope::LOWER_48,
};

#[derive(sqlx::FromRow)]
pub struct LocalContactStop {
    pub address: String,
    pub cursor: Value,
    pub attempts: i32,
    pub detail: Option<String>,
    pub consistent_attempt: bool,
    pub operations: i64,
    pub attempts_recorded: i64,
    pub wires: i64,
    pub redirects: i64,
    pub captures: bool,
    pub origin_denied: bool,
}

fn canonical(address: &str) -> Option<Url> {
    let url = Url::parse(address).ok()?;
    let site = url.host_str()?.strip_suffix(".milesplit.com")?;
    (url.scheme() == "https"
        && url.as_str() == address
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && !url.path().contains('%')
        && (site == "www" || LOWER_48.contains(&site.to_ascii_uppercase().as_str())))
    .then_some(url)
}

fn identifier(value: &str) -> bool {
    value.split('-').next().is_some_and(|id| {
        !id.starts_with('0')
            && id.parse::<u64>().is_ok_and(|id| id > 0)
            && id.bytes().all(|byte| byte.is_ascii_digit())
    })
}

pub fn admin_route(address: &str) -> bool {
    canonical(address).is_some_and(|url| {
        matches!(url.path().split('/').take(7).collect::<Vec<_>>().as_slice(),
            ["", "teams", team, admin, ..] if identifier(team) && admin.eq_ignore_ascii_case("admin"))
    })
}

fn public_query(url: &Url) -> bool {
    let pairs = url.query_pairs().take(6).collect::<Vec<_>>();
    pairs.len() <= 5
        && pairs.iter().enumerate().all(|(index, (key, value))| {
            !pairs.iter().take(index).any(|(seen, _)| seen == key)
                && match key.as_ref() {
                    "page" => value
                        .parse::<u32>()
                        .is_ok_and(|n| (1..=100_000).contains(&n)),
                    "year" => value
                        .parse::<u32>()
                        .is_ok_and(|n| (1900..=2200).contains(&n)),
                    "season" => matches!(value.as_ref(), "cc" | "indoor" | "outdoor"),
                    "sport" => matches!(value.as_ref(), "cc" | "xc" | "tf" | "indoor" | "outdoor"),
                    "level" => value == "hs",
                    _ => false,
                }
        })
}

pub fn public_origin(address: &str) -> Option<String> {
    let url = canonical(address)?;
    let parts = url
        .path()
        .trim_end_matches('/')
        .split('/')
        .take(7)
        .collect::<Vec<_>>();
    let public = match parts.as_slice() {
        ["", "teams" | "results"] => true,
        ["", "athletes" | "teams" | "meets", id]
        | ["", "athletes", id, "stats" | "results" | "progression"]
        | ["", "teams", id, "roster" | "results"]
        | ["", "meets", id, "results"] => identifier(id),
        ["", "meets", id, "results", file]
        | ["", "meets", id, "results", file, "raw" | "formatted"] => {
            identifier(id) && identifier(file)
        }
        _ => false,
    };
    (public
        && public_query(&url)
        && !crate::transport::denied_url(address)
        && crate::transport::robots::check(address, address).is_ok())
    .then(|| url.origin().ascii_serialization())
}

impl LocalContactStop {
    pub fn qualifies(&self, origin: &str) -> bool {
        let Ok(target) = serde_json::from_value::<Target>(self.cursor.clone()) else {
            return false;
        };
        target.source == Source::MileSplit
            && target.kind == PageKind::Contacts
            && target.url == self.address
            && target.method.is_none()
            && target.body.is_none()
            && target.token_url.is_none()
            && target.browser_origin.is_none()
            && admin_route(&self.address)
            && canonical(&self.address).is_some_and(|url| url.query().is_none() && url.path().split('/').count()==4 && url.path().ends_with("/admin") && url.origin().ascii_serialization() == origin)
            && self.cursor.get("access_stop_url").and_then(Value::as_str) == Some(origin)
            && self.detail.as_deref() == Some(format!(
                "browser interception failed: source access stopped at {origin} (HTTP None); human review required"
            ).as_str())
            && self.attempts == 1
            && self.consistent_attempt
            && self.operations == 1
            && self.attempts_recorded == 1
            && self.wires == 1
            && self.redirects == 1
            && !self.captures
            && !self.origin_denied
    }
}

#[cfg(test)]
mod tests;
