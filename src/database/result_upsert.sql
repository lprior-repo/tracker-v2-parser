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
    represented_school_id = CASE WHEN $26 THEN NULL
        ELSE COALESCE(EXCLUDED.represented_school_id, results.represented_school_id) END,
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
    raw = (results.raw
        - CASE WHEN btrim(EXCLUDED.raw_mark) <> ''
            THEN ARRAY['reported_status'] ELSE ARRAY[]::text[] END
        - CASE WHEN $26 THEN ARRAY['school_id'] ELSE ARRAY[]::text[] END) || COALESCE((
        SELECT jsonb_object_agg(key,
            CASE WHEN key = 'source_values' AND jsonb_typeof(value) = 'object' THEN
                (CASE WHEN jsonb_typeof(results.raw->'source_values') = 'object'
                    THEN results.raw->'source_values' ELSE '{}'::jsonb END) || COALESCE((
                    SELECT jsonb_object_agg(evidence.key, CASE
                        WHEN evidence.key IN ('eventType', 'row', 'context')
                         AND jsonb_typeof(evidence.value) = 'object' THEN
                            (CASE WHEN jsonb_typeof(results.raw->'source_values'->evidence.key) = 'object'
                                THEN results.raw->'source_values'->evidence.key ELSE '{}'::jsonb END) || COALESCE((
                                SELECT jsonb_object_agg(nested.key, CASE
                                    WHEN evidence.key = 'row' AND nested.key = 'attributes'
                                     AND jsonb_typeof(nested.value) = 'object' THEN
                                        (CASE WHEN jsonb_typeof(results.raw #> '{source_values,row,attributes}') = 'object'
                                            THEN results.raw #> '{source_values,row,attributes}' ELSE '{}'::jsonb END) || COALESCE((
                                            SELECT jsonb_object_agg(attribute.key, attribute.value)
                                            FROM jsonb_each(nested.value) AS attribute(key, value)
                                            WHERE attribute.value <> 'null'::jsonb
                                              AND (jsonb_typeof(attribute.value) <> 'string' OR btrim(attribute.value #>> '{}') <> '')
                                        ), '{}'::jsonb)
                                    ELSE nested.value END)
                                FROM jsonb_each(evidence.value) AS nested(key, value)
                                WHERE nested.value <> 'null'::jsonb
                                  AND (jsonb_typeof(nested.value) <> 'string' OR btrim(nested.value #>> '{}') <> '')
                            ), '{}'::jsonb)
                        ELSE evidence.value END)
                    FROM jsonb_each(value) AS evidence(key, value)
                    WHERE evidence.value <> 'null'::jsonb
                      AND (jsonb_typeof(evidence.value) <> 'string' OR btrim(evidence.value #>> '{}') <> '')
                ), '{}'::jsonb) || CASE WHEN $27::jsonb IS NOT NULL
                    THEN $27::jsonb
                    ELSE '{}'::jsonb END
            ELSE value END)
        FROM jsonb_each(EXCLUDED.raw)
        WHERE value <> 'null'::jsonb
          AND (jsonb_typeof(value) <> 'string' OR btrim(value #>> '{}') <> '')
    ), '{}'::jsonb),
    source_url = EXCLUDED.source_url, last_seen = clock_timestamp(),
    season = COALESCE(EXCLUDED.season, results.season),
    grade = CASE WHEN btrim(EXCLUDED.grade) <> '' THEN EXCLUDED.grade ELSE results.grade END,
    gender = CASE WHEN btrim(EXCLUDED.gender) <> '' THEN EXCLUDED.gender ELSE results.gender END,
    normalized_numeric = EXCLUDED.normalized_numeric,
    normalized_unit = EXCLUDED.normalized_unit,
    specification = EXCLUDED.specification,
    status = EXCLUDED.status
WHERE results.athlete_source_id IS NULL OR results.athlete_source_id = EXCLUDED.athlete_source_id
RETURNING id
