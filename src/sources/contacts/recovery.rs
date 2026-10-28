use anyhow::Result;

use crate::model::{PageKind, Target};

pub fn after_not_found(target: &Target) -> Result<Option<Target>> {
    if target.kind != PageKind::Contacts
        || target.context.subject_id.as_deref() != Some("contacts-depth:0")
        || target
            .context
            .school_id
            .as_deref()
            .is_none_or(|id| id.trim().is_empty())
        || target
            .context
            .school_name
            .as_deref()
            .is_none_or(|name| name.trim().is_empty())
        || target
            .method
            .as_deref()
            .is_some_and(|method| method != "GET")
        || target.body.is_some()
    {
        return Ok(None);
    }
    let mut url = super::public_url(&target.url)?;
    if url.path() == "/" && url.query().is_none() {
        return Ok(None);
    }
    url.set_path("/");
    url.set_query(None);
    url.set_fragment(None);
    let mut root = target.clone();
    root.url = url.into();
    root.method = None;
    root.body = None;
    root.browser_origin = None;
    root.token_url = None;
    Ok(Some(root))
}
