use std::sync::LazyLock;

use anyhow::{Context, Result};
use regex::Regex;

static SENSITIVE: LazyLock<Result<Regex, regex::Error>> = LazyLock::new(|| {
    Regex::new(
        r"(?i)password|authorization|cookie|jwt|api[_-]?key|access[_-]?key|credential|secret|bearer|(?:^|[?&\s])token(?:[=:])|\beyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.",
    )
});
static URL: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r#"https?://[^\s<>"']+"#));

pub(super) fn url(value: &str) -> Result<String> {
    let Ok(mut url) = url::Url::parse(value) else {
        return Ok("[Invalid checkpoint URL; inspect checkpoint locally]".into());
    };
    if !matches!(url.scheme(), "https" | "http") {
        return Ok("[Non-public checkpoint URL omitted]".into());
    }
    url.set_username("")
        .map_err(|()| anyhow::anyhow!("cannot sanitize checkpoint username"))?;
    url.set_password(None)
        .map_err(|()| anyhow::anyhow!("cannot sanitize checkpoint password"))?;
    url.set_fragment(None);
    let sensitive = SENSITIVE
        .as_ref()
        .context("compiling review credential filter")?;
    let pairs = url
        .query_pairs()
        .filter(|(key, value)| {
            !sensitive.is_match(key)
                && !["token", "auth"]
                    .iter()
                    .any(|part| key.to_ascii_lowercase().contains(part))
                && !sensitive.is_match(value)
        })
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    url.set_query(None);
    if !pairs.is_empty() {
        url.query_pairs_mut().extend_pairs(pairs);
    }
    Ok(url.into())
}

pub(super) fn error(value: &str) -> Result<String> {
    let sensitive = SENSITIVE
        .as_ref()
        .context("compiling review credential filter")?;
    if sensitive.is_match(value) {
        return Ok("[Sensitive request details omitted; inspect checkpoint locally]".into());
    }
    if value.contains('<') || value.trim_start().starts_with(['{', '[']) {
        return Ok("[Response payload omitted; inspect retained capture locally]".into());
    }
    let urls = URL.as_ref().context("compiling review URL filter")?;
    let (mut text, end) = urls.find_iter(value).try_fold(
        (String::new(), 0),
        |(mut output, end), found| -> Result<_> {
            output.push_str(
                value
                    .get(end..found.start())
                    .context("review error URL boundary")?,
            );
            output.push_str(&url(found.as_str())?);
            Ok((output, found.end()))
        },
    )?;
    text.push_str(value.get(end..).context("review error text boundary")?);
    Ok(text)
}
