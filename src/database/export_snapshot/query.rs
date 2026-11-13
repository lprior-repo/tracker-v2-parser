use std::sync::Arc;

#[derive(Clone, Copy)]
pub(super) enum Projection {
    Complete,
    Workbook,
}

pub(super) fn statement(projection: Projection) -> Arc<str> {
    let records = include_str!("../athlete_records.sql")
        .replacen(
            "WHERE p.school_id IS NOT NULL",
            "WHERE p.athlete_id = $3::bigint AND p.school_id IS NOT NULL",
            1,
        )
        .replacen(
            "WHERE a.name IS NOT NULL",
            "WHERE a.id = $3::bigint AND a.name IS NOT NULL",
            1,
        );
    match projection {
        Projection::Complete => format!(
            "SELECT CASE WHEN octet_length(document::text) <= $4::bigint THEN document::text END, \
             octet_length(document::text)::bigint FROM ({records}) report(document)"
        ),
        Projection::Workbook => include_str!("workbook.sql").replace("__RECORDS__", &records),
    }
    .into()
}
