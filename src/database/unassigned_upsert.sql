INSERT INTO results (
    source_result_key, represented_school_id, source, source_result_id,
    event, raw_mark, sport, meet_id, meet_date, status, is_personal_record,
    is_relay, source_url, raw, normalized_numeric, normalized_unit, specification
)
VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,FALSE,$11,$12,$13,$14,$15,$16)
ON CONFLICT (source_result_key) DO UPDATE SET
    represented_school_id = COALESCE(EXCLUDED.represented_school_id, results.represented_school_id),
    event = EXCLUDED.event, raw_mark = EXCLUDED.raw_mark, sport = EXCLUDED.sport,
    meet_id = EXCLUDED.meet_id, meet_date = EXCLUDED.meet_date,
    status = EXCLUDED.status, is_relay = EXCLUDED.is_relay,
    source_url = EXCLUDED.source_url, raw = EXCLUDED.raw, last_seen = clock_timestamp(),
    normalized_numeric = EXCLUDED.normalized_numeric, normalized_unit = EXCLUDED.normalized_unit,
    specification = EXCLUDED.specification
WHERE results.athlete_source_id IS NULL
