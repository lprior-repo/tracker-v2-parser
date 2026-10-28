use anyhow::Result;

use crate::{
    model::{PageContext, PageKind, Source, Target},
    transport::policy,
};

pub(super) async fn validate_connect(
    authority: &str,
    #[cfg(test)] local_origins: &[url::Origin],
) -> Result<()> {
    let address = format!("https://{authority}/");
    #[cfg(test)]
    {
        let route = url::Url::parse(&address)?;
        if local_origins.iter().any(|origin| {
            url::Url::parse(&origin.ascii_serialization()).is_ok_and(|safe| {
                safe.host_str() == route.host_str()
                    && safe.port_or_known_default() == route.port_or_known_default()
            })
        }) {
            policy::check_url_secrets(&route)?;
            return Ok(());
        }
    }
    let target = Target::get(
        Source::AthleticNet,
        PageKind::Contacts,
        address,
        PageContext::default(),
    );
    let url = policy::source_url(&target, &target.url)?;
    drop(policy::public_addresses(&url).await?);
    Ok(())
}
