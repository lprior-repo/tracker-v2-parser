use anyhow::Result;
use scraper::{ElementRef, Html};

use super::{AccessDenied, normalized, selector, text, text_content};

pub(super) fn check(document: &Html, url: &str) -> Result<()> {
    let titles = selector("title")?;
    let headings = selector("h1")?;
    let primary = document.select(&headings).find(|heading| {
        !heading
            .ancestors()
            .filter_map(ElementRef::wrap)
            .any(|ancestor| {
                let element = ancestor.value();
                matches!(
                    element.name(),
                    "footer" | "aside" | "nav" | "dialog" | "form"
                ) || element.attr("hidden").is_some()
                    || element
                        .attr("aria-hidden")
                        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
            })
    });
    let challenged = document
        .select(&titles)
        .take(1)
        .chain(primary)
        .any(|heading| {
            let heading = normalized(&text(heading));
            [
                "accessdenied",
                "justamoment",
                "attentionrequired",
                "verifyyouarehuman",
                "verifythatyouarehuman",
                "checkingyourbrowser",
                "captchachallenge",
                "signin",
                "login",
                "subscribe",
            ]
            .iter()
            .any(|marker| heading.starts_with(marker))
        });
    let challenge_form = selector("#challenge-form, #cf-challenge-running")?;
    if challenged
        || rendered_challenge(document)?
        || crate::transport::denied_url(url)
        || document.select(&challenge_form).next().is_some()
    {
        return Err(AccessDenied.into());
    }
    Ok(())
}

fn rendered_challenge(document: &Html) -> Result<bool> {
    let bodies = selector("body")?;
    let Some(body) = document.select(&bodies).next() else {
        return Ok(false);
    };
    let mut visible = text_content(
        body,
        &["footer", "header", "nav", "aside", "form", "dialog"],
    );
    visible.make_ascii_lowercase();
    Ok([
        "verify you are human",
        "verify that you are human",
        "checking your browser",
        "captcha challenge",
    ]
    .iter()
    .any(|marker| visible.contains(marker)))
}
