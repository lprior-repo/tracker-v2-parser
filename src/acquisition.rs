use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use crate::{
    model::{PageKind, Source, Target},
    scope::LOWER_48,
};

pub(crate) const EXCLUSION: &str = "public-results/v1";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum AcquisitionPolicy {
    #[default]
    Complete,
    PublicResults {
        #[serde(default)]
        reviewed_media_stops: Vec<ReviewedMediaStop>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ReviewedMediaStop(String);

impl TryFrom<String> for ReviewedMediaStop {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self> {
        ensure!(value.len() <= 8192, "media review URL exceeds 8192 bytes");
        let url = Url::parse(&value).context("parsing reviewed media stop")?;
        ensure!(
            url.scheme() == "https"
                && url.username().is_empty()
                && url.password().is_none()
                && url.port().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && url.as_str() == value
                && !url.path().contains('%'),
            "media review must be a canonical uncredentialed HTTPS URL"
        );
        let site = url
            .host_str()
            .and_then(|host| host.strip_suffix(".milesplit.com"))
            .context("media review must belong to MileSplit")?;
        ensure!(
            site == "www" || LOWER_48.contains(&site.to_ascii_uppercase().as_str()),
            "unsupported reviewed MileSplit host"
        );
        let parts = url.path().split('/').collect::<Vec<_>>();
        let valid = match parts.as_slice() {
            ["", "athletes", athlete, "photos" | "videos"] => {
                athlete.split('-').next().is_some_and(|id| {
                    !id.starts_with('0')
                        && id.bytes().all(|b| b.is_ascii_digit())
                        && id.parse::<u64>().is_ok_and(|id| id > 0)
                })
            }
            _ => false,
        };
        ensure!(
            valid,
            "only exact athlete photo/video stops may be reviewed"
        );
        Ok(Self(value))
    }
}

impl From<ReviewedMediaStop> for String {
    fn from(value: ReviewedMediaStop) -> Self {
        value.0
    }
}

impl AcquisitionPolicy {
    pub(crate) fn validate(&self) -> Result<()> {
        if let Self::PublicResults {
            reviewed_media_stops,
        } = self
        {
            ensure!(
                reviewed_media_stops.len() <= 32,
                "at most 32 media stops may be reviewed"
            );
        }
        Ok(())
    }

    pub(crate) const fn includes_contacts(&self) -> bool {
        matches!(self, Self::Complete)
    }

    pub(crate) fn exclusion_reason(&self, target: &Target) -> Option<&'static str> {
        if matches!(self, Self::Complete) {
            return None;
        }
        if target.kind == PageKind::Contacts {
            return Some("Public-results scope excludes external school-contact acquisition");
        }
        if target.source != Source::MileSplit {
            return None;
        }
        if crate::transport::robots::check(&target.url, &target.url).is_err() {
            return Some(
                "Public-results scope excludes a published robots restriction or ambiguous route",
            );
        }
        Url::parse(&target.url).ok().and_then(|url| {
            if url.path().contains('%') { return Some("Public-results scope excludes encoded acquisition paths"); }
            url.path_segments().is_some_and(|parts| parts.into_iter().any(|part| matches!(part, "photos" | "videos")))
                .then_some("Public-results scope excludes photo/video acquisition; published metadata remains retained")
        })
    }

    pub(crate) fn permits_preserved_stop(
        &self,
        address: &str,
        attempts: i32,
        cursor: &Value,
    ) -> Result<bool> {
        let Self::PublicResults {
            reviewed_media_stops,
        } = self
        else {
            return Ok(false);
        };
        let target: Target =
            serde_json::from_value(cursor.clone()).context("decoding preserved source stop")?;
        ensure!(
            target.url == address,
            "blocked checkpoint URL does not match its target"
        );
        if target.source == Source::MileSplit
            && target.kind == PageKind::Athlete
            && reviewed_media_stops
                .iter()
                .any(|review| review.0 == address)
        {
            return Ok(true);
        }
        Ok(attempts == 0
            && cursor
                .get("public_results_exclusion")
                .and_then(Value::as_str)
                == Some(EXCLUSION)
            && self.exclusion_reason(&target).is_some())
    }
}
