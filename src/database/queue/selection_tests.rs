use super::{DurableSelection, FallbackScope, selection_query};
use crate::model::Source;

const DURABLE: DurableSelection<'static> = DurableSelection {
    generation: "query-dispatch-test",
    now_ms: 0,
    owned: &[],
    scope: FallbackScope::MileSplitPublicDiscovery,
};

#[test]
fn given_durable_public_milesplit_scope_when_source_is_unrestricted_or_milesplit_then_scoped_sql_is_selected()
 {
    assert_eq!(
        selection_query(None, Some(DURABLE)),
        include_str!("selection_scoped.sql")
    );
    assert_eq!(
        selection_query(Some(Source::MileSplit), Some(DURABLE)),
        include_str!("selection_scoped.sql")
    );
}

#[test]
fn given_explicit_athleticnet_source_when_scope_is_public_milesplit_then_legacy_sql_is_selected() {
    assert_eq!(
        selection_query(Some(Source::AthleticNet), Some(DURABLE)),
        include_str!("selection.sql")
    );
}

#[test]
fn given_all_or_non_durable_scope_when_source_is_unrestricted_or_milesplit_then_legacy_sql_is_selected()
 {
    let all = DurableSelection {
        scope: FallbackScope::All,
        ..DURABLE
    };
    assert_eq!(
        selection_query(None, Some(all)),
        include_str!("selection.sql")
    );
    assert_eq!(
        selection_query(Some(Source::MileSplit), Some(all)),
        include_str!("selection.sql")
    );
    assert_eq!(selection_query(None, None), include_str!("selection.sql"));
    assert_eq!(
        selection_query(Some(Source::MileSplit), None),
        include_str!("selection.sql")
    );
}
