import json

from recovery_support import sql


HEADERS = [
    "Checkpoint ID", "Source", "Kind", "State", "School", "School source ID", "Athlete",
    "Athlete source ID", "URL", "Status", "Attempts", "Durable state", "Retry after UTC",
    "Error", "Last success", "Created",
    "Next action", "AI guidance",
]
GUIDANCE = ("Use public evidence and exact source IDs. Do not bypass login, paywalls, "
    "challenges or saved stops; do not infer missing results.")


def seed_review(database):
    examples = [
        ("b", "failed", "Owned Wisconsin Track Fixture", "review-track-school",
            "https://review-fixture.invalid/wisconsin/contact-failure?program=track",
            "Contact parser rejected the isolated school fixture.",
            "Investigate checkpoint and retained evidence before an authorized retry."),
        ("c", "blocked", "Owned Wisconsin XC Fixture", "review-xc-school",
            "https://review-fixture.invalid/wisconsin/contact-challenge?program=xc",
            "Access denied: public page challenge requires human handling.",
            "Human handling required; preserve the access stop."),
    ]
    created = "2026-10-29T12:34:56.123456Z"
    rows = []
    for key, status, school, school_id, url, error, action in examples:
        target = dict(source="milesplit", kind="contacts", url=url,
            context=dict(state="WI", school_id=school_id, school_name=school, season=2026))
        literals = [json.dumps(target), url, status, error, created]
        cursor, url_sql, status_sql, error_sql, created_sql = [
            "'" + value.replace("'", "''") + "'" for value in literals]
        checkpoint = sql(database, "INSERT INTO scrape_pages"
            "(source,page_key,url,cursor,status,attempts,last_error,created_at) VALUES"
            "('milesplit',repeat('" + key + "',64)," + url_sql + "," + cursor
            + "::jsonb," + status_sql + ",0," + error_sql + "," + created_sql
            + "::timestamptz) RETURNING id;")
        values = [str(checkpoint), "milesplit", "contacts", "WI", school, school_id,
            "", "", url, status, "0", "", "", error, "", created, action, GUIDANCE]
        rows.append(dict(zip(HEADERS, values)))
    return rows


def verify_review(rows, namespace, strings, decode_row, expected):
    if len(expected) != 2 or [row["Status"] for row in expected] != ["failed", "blocked"]:
        raise RuntimeError("owned Review proof must retain one failed and one blocked checkpoint")
    if len(rows) != 3:
        raise RuntimeError("Review must contain exactly two isolated Wisconsin checkpoint rows")
    decoded = [decode_row(row, namespace, strings) for row in rows]
    if list(decoded[0].values()) != HEADERS:
        raise RuntimeError("Review headers differ from the exact recovery proof contract")
    actual = [{title: row.get(column, "") for column, title in decoded[0].items()}
        for row in decoded[1:]]
    if actual != expected:
        raise RuntimeError("Review checkpoint cells differ from the exact seeded evidence")
    if any("<" in value or ">" in value for row in actual for value in row.values()):
        raise RuntimeError("Review contains raw response markup")
    return {"rows_including_header": len(rows), "headers": HEADERS, "checkpoints": actual,
        "context_only_school_names_preserved": True, "no_raw_response_markup": True}
