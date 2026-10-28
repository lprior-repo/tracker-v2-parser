use anyhow::Result;
use track_scrapper_v2::sources::wiaa;

fn empty() -> &'static str {
    "<label id='ajaxMsgText'>Nothing was found matching this search criteria</label><input id='SearchLetter' value='Q'><input id='SearchType' value='1'>"
}

#[test]
fn given_verified_empty_wiaa_letter_when_parsed_then_empty_is_accepted_without_accepting_unrelated_missing_pages()
-> Result<()> {
    assert!(wiaa::parse_listing(empty())?.is_empty());
    for body in [
        empty().replace("id='ajaxMsgText'", "id='unrelated'"),
        empty().replace("value='Q'", "value='QQ'"),
        empty().replace("value='Q'", "value='q'"),
        empty().replace("value='1'", "value='2'"),
        format!("{}<input id='SearchLetter' value='R'>", empty()),
        format!(
            "{}<label id='ajaxMsgText'>Nothing was found matching this search criteria</label>",
            empty()
        ),
        format!("{}<table id='tblSchools'><tbody></tbody></table>", empty()),
        format!("<title>Just a moment...</title>{}", empty()),
        format!("<h1>Verify you are human</h1>{}", empty()),
    ] {
        assert!(wiaa::parse_listing(&body).is_err(), "{body}");
    }
    assert!(wiaa::parse_listing("<main>Unrelated page</main>").is_err());
    Ok(())
}
