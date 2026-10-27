use anyhow::Result;
use std::collections::HashSet;
use track_scrapper_v2::model::{PageKind, Sport};
use track_scrapper_v2::sources::milesplit::{meet_target, parse};

const AR: &str = include_str!("../fixtures/milesplit/meet-ar-770517-results.html");
const DE: &str = include_str!("../fixtures/milesplit/meet-de-770101-results.html");

#[test]
fn given_arkansas_published_table_when_parsed_then_every_row_and_duplicate_is_retained()
-> Result<()> {
    let mut target = meet_target("770517", Sport::Outdoor)?;
    target.url =
        "https://ar.milesplit.com/meets/770517-arkansas-greatest-athlete-2026/results".to_owned();
    let page = parse(&target, AR)?;
    assert_eq!(page.unassigned_results.len(), 175);
    assert!(page.athletes.is_empty());
    assert!(page.performances.is_empty());
    let first = &page.unassigned_results[0];
    assert_eq!(
        first.event,
        "Womens High School (15-18) 100 Meters - Finals"
    );
    assert_eq!(first.raw["name"], "Aja Colson");
    assert_eq!(first.mark, "12.74");
    assert!(
        first.raw["published_html"]
            .as_str()
            .is_some_and(|row| row.contains("Aja Colson"))
    );
    assert_eq!(page.unassigned_results[1].raw["place"], "");
    assert_eq!(page.unassigned_results[1].mark, "DNS");
    let duplicates: Vec<_> = page
        .unassigned_results
        .iter()
        .filter(|row| row.raw["name"] == "Isabella Sategna" && row.mark == "23.85")
        .collect();
    assert_eq!(duplicates.len(), 2);
    assert_ne!(duplicates[0].result_id, duplicates[1].result_id);
    assert_eq!(
        page.unassigned_results
            .last()
            .map(|row| row.raw["name"].as_str()),
        Some(Some("Blade Kuenzi"))
    );
    let ids: HashSet<_> = page
        .unassigned_results
        .iter()
        .map(|row| row.result_id.as_deref())
        .collect();
    assert_eq!(ids.len(), 175);
    Ok(())
}

#[test]
fn given_delaware_published_table_when_parsed_then_rows_and_other_inventory_survive() -> Result<()>
{
    let mut target = meet_target("770101", Sport::CrossCountry)?;
    target.url =
        "https://de.milesplit.com/meets/770101-spartan-moonlight-showcase-2026/results".to_owned();
    let page = parse(&target, DE)?;
    assert_eq!(page.unassigned_results.len(), 1545);
    assert_eq!(
        page.unassigned_results[0].event,
        "Womens F - JV Gold 4000.00"
    );
    assert_eq!(page.unassigned_results[0].raw["name"], "Jordan Koss");
    assert_eq!(page.unassigned_results[0].mark, "16:58.80");
    let last = page
        .unassigned_results
        .last()
        .ok_or_else(|| anyhow::anyhow!("missing result"))?;
    assert_eq!(last.raw["name"], "Radhika Viswanath");
    assert_eq!(last.mark, "DQ");
    assert_eq!(last.raw["place"], "");
    assert!(
        page.next
            .iter()
            .any(|next| next.url.contains("/1322164/raw"))
    );
    assert!(
        !page
            .next
            .iter()
            .any(|next| next.url.contains("/1322093/raw"))
    );
    let mut result = target.clone();
    result.kind = PageKind::Results;
    result.url = "https://de.milesplit.com/meets/770101/results/1322093/raw".to_owned();
    let alias = parse(&result, DE)?;
    assert_eq!(
        alias.unassigned_results[0].result_id,
        page.unassigned_results[0].result_id
    );
    Ok(())
}

#[test]
fn given_damaged_published_table_when_parsed_then_unknown_rows_remain_errors() -> Result<()> {
    let mut target = meet_target("770517", Sport::Outdoor)?;
    target.url =
        "https://ar.milesplit.com/meets/770517-arkansas-greatest-athlete-2026/results".to_owned();
    let header = "<tr><td>Place</td><td>Name</td><td>Grade</td><td>Team</td><td>Mark</td></tr>";
    let row =
        "<tr><td>1</td><td>Aja Colson</td><td></td><td>Xcel Performance</td><td>12.74</td></tr>";
    for (columns, result) in [
        ("<tr><td>Unknown</td></tr>", row),
        (
            header,
            "<tr><td>1</td><td></td><td></td><td>Xcel Performance</td><td>12.74</td></tr>",
        ),
        ("", row),
    ] {
        let body = format!(
            "<div id='meetResultsBody'><table>\
             <tr><td>AthleticRUNMEET</td></tr>\
             <tr><td>Arkansas Greatest Athlete</td></tr>\
             <tr><td></td><td>(updated 07/23/2026)</td></tr>\
             <tr><td>Womens 100 Meters - Finals</td></tr>{columns}{result}\
             </table></div>"
        );
        assert!(parse(&target, &body).is_err());
    }
    Ok(())
}
