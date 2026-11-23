#[derive(Debug)]
pub enum PageDataRejected {
    UnrepresentableText,
    OversizedIndexValue,
    ConflictingResultOwner,
}

impl std::fmt::Display for PageDataRejected {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::UnrepresentableText => "source page contains text PostgreSQL cannot represent",
            Self::OversizedIndexValue => {
                "source page contains a value larger than PostgreSQL can index"
            }
            Self::ConflictingResultOwner => {
                "source result ID belongs to a different athlete; refusing to overwrite its identity"
            }
        })
    }
}

impl std::error::Error for PageDataRejected {}

pub(super) fn classify(error: anyhow::Error) -> anyhow::Error {
    let rejection = error
        .chain()
        .filter_map(|cause| {
            cause
                .downcast_ref::<sqlx::Error>()
                .and_then(sqlx::Error::as_database_error)
                .and_then(|database| database.try_downcast_ref::<sqlx::postgres::PgDatabaseError>())
        })
        .find_map(
            |database| match (database.code(), database.file(), database.routine()) {
                ("22021" | "22P05", _, _) => Some(PageDataRejected::UnrepresentableText),
                ("54000", Some("nbtutils.c"), Some("_bt_check_third_page"))
                | ("54000", Some("indextuple.c"), Some("index_form_tuple_context")) => {
                    Some(PageDataRejected::OversizedIndexValue)
                }
                _ => None,
            },
        );
    if let Some(rejection) = rejection {
        error.context(rejection)
    } else {
        error
    }
}

#[cfg(test)]
mod tests;
