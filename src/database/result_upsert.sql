INSERT INTO results (
    source_result_key, athlete_source_id, athlete_id, represented_school_id,
    source, source_result_id, event, raw_mark, sport, meet_id, meet_name,
    meet_date, wind, timing, course, is_personal_record, is_relay, raw,
    source_url, season, grade, gender, normalized_numeric, normalized_unit,
    specification, status
)
VALUES (
    $1, $2, (SELECT athlete_id FROM athlete_sources WHERE id = $2), $3,
    $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17,
    $18, $19, $20, $21, $22, $23, $24, $25
)
ON CONFLICT (source_result_key) DO UPDATE SET
    athlete_source_id = EXCLUDED.athlete_source_id,
    athlete_id = EXCLUDED.athlete_id,
    represented_school_id = COALESCE(EXCLUDED.represented_school_id, results.represented_school_id),
    event = CASE WHEN btrim(EXCLUDED.event) <> '' THEN EXCLUDED.event ELSE results.event END,
    raw_mark = CASE WHEN btrim(EXCLUDED.raw_mark) <> '' THEN EXCLUDED.raw_mark ELSE results.raw_mark END,
    sport = EXCLUDED.sport,
    meet_id = CASE WHEN btrim(EXCLUDED.meet_id) <> '' THEN EXCLUDED.meet_id ELSE results.meet_id END,
    meet_name = CASE WHEN btrim(EXCLUDED.meet_name) <> '' THEN EXCLUDED.meet_name ELSE results.meet_name END,
    meet_date = CASE WHEN btrim(EXCLUDED.meet_date) <> '' THEN EXCLUDED.meet_date ELSE results.meet_date END,
    wind = COALESCE(EXCLUDED.wind, results.wind),
    timing = CASE WHEN btrim(EXCLUDED.timing) <> '' THEN EXCLUDED.timing ELSE results.timing END,
    course = CASE WHEN btrim(EXCLUDED.course) <> '' THEN EXCLUDED.course ELSE results.course END,
    is_personal_record = EXCLUDED.is_personal_record, is_relay = EXCLUDED.is_relay,
    raw = results.raw || COALESCE((
        SELECT jsonb_object_agg(key, value)
        FROM jsonb_each(EXCLUDED.raw)
        WHERE value <> 'null'::jsonb
          AND (jsonb_typeof(value) <> 'string' OR btrim(value #>> '{}') <> '')
    ), '{}'::jsonb),
    source_url = EXCLUDED.source_url, last_seen = clock_timestamp(),
    season = COALESCE(EXCLUDED.season, results.season),
    grade = CASE WHEN btrim(EXCLUDED.grade) <> '' THEN EXCLUDED.grade ELSE results.grade END,
    gender = CASE WHEN btrim(EXCLUDED.gender) <> '' THEN EXCLUDED.gender ELSE results.gender END,
    normalized_numeric = CASE
        WHEN btrim(EXCLUDED.event) <> '' AND btrim(EXCLUDED.raw_mark) <> '' THEN EXCLUDED.normalized_numeric
        WHEN (btrim(EXCLUDED.event) = '' OR EXCLUDED.event = results.event)
         AND (btrim(EXCLUDED.raw_mark) = '' OR EXCLUDED.raw_mark = results.raw_mark)
         AND EXCLUDED.is_relay = results.is_relay THEN results.normalized_numeric
        ELSE NULL
    END,
    normalized_unit = CASE
        WHEN btrim(EXCLUDED.event) <> '' AND btrim(EXCLUDED.raw_mark) <> '' THEN EXCLUDED.normalized_unit
        WHEN (btrim(EXCLUDED.event) = '' OR EXCLUDED.event = results.event)
         AND (btrim(EXCLUDED.raw_mark) = '' OR EXCLUDED.raw_mark = results.raw_mark)
         AND EXCLUDED.is_relay = results.is_relay THEN results.normalized_unit
        ELSE NULL
    END,
    specification = CASE WHEN btrim(EXCLUDED.event) = '' THEN results.specification ELSE EXCLUDED.specification END,
    status = CASE
        WHEN btrim(EXCLUDED.event) <> '' AND btrim(EXCLUDED.raw_mark) <> '' THEN EXCLUDED.status
        WHEN (btrim(EXCLUDED.event) = '' OR EXCLUDED.event = results.event)
         AND (btrim(EXCLUDED.raw_mark) = '' OR EXCLUDED.raw_mark = results.raw_mark)
         AND EXCLUDED.is_relay = results.is_relay THEN results.status
        WHEN btrim(EXCLUDED.raw_mark) <> '' THEN EXCLUDED.status
        WHEN results.status = 'recorded' THEN 'unclassified'
        ELSE results.status
    END
WHERE results.athlete_source_id IS NULL OR results.athlete_source_id = EXCLUDED.athlete_source_id
RETURNING id
