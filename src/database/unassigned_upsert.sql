INSERT INTO results (
    source_result_key, represented_school_id, source, source_result_id,
    event, raw_mark, sport, meet_id, meet_date, status, is_personal_record,
    is_relay, source_url, raw, normalized_numeric, normalized_unit, specification
)
VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,FALSE,$11,$12,$13,$14,$15,$16)
ON CONFLICT (source_result_key) DO UPDATE SET
    athlete_source_id = CASE WHEN EXCLUDED.is_relay THEN NULL ELSE results.athlete_source_id END,
    athlete_id = CASE WHEN EXCLUDED.is_relay THEN NULL ELSE results.athlete_id END,
    represented_school_id = COALESCE(EXCLUDED.represented_school_id, results.represented_school_id),
    event = CASE WHEN btrim(EXCLUDED.event) <> '' THEN EXCLUDED.event ELSE results.event END,
    raw_mark = CASE WHEN btrim(EXCLUDED.raw_mark) <> '' THEN EXCLUDED.raw_mark ELSE results.raw_mark END,
    sport = EXCLUDED.sport,
    meet_id = CASE WHEN btrim(EXCLUDED.meet_id) <> '' THEN EXCLUDED.meet_id ELSE results.meet_id END,
    meet_date = CASE WHEN btrim(EXCLUDED.meet_date) <> '' THEN EXCLUDED.meet_date ELSE results.meet_date END,
    status = EXCLUDED.status,
    is_relay = EXCLUDED.is_relay,
    source_url = EXCLUDED.source_url,
    raw = (CASE WHEN btrim(EXCLUDED.raw_mark) <> ''
        THEN results.raw - 'reported_status' ELSE results.raw END) || COALESCE((
        SELECT jsonb_object_agg(key, value)
        FROM jsonb_each(EXCLUDED.raw)
        WHERE key <> 'raw'
          AND value <> 'null'::jsonb
          AND (jsonb_typeof(value) <> 'string' OR btrim(value #>> '{}') <> '')
    ), '{}'::jsonb) || jsonb_build_object('raw',
        (CASE WHEN btrim(EXCLUDED.raw_mark) <> ''
            THEN COALESCE(results.raw->'raw', '{}'::jsonb) - 'statusCode'
            ELSE COALESCE(results.raw->'raw', '{}'::jsonb) END)
        || COALESCE((
            SELECT jsonb_object_agg(key,
                CASE WHEN key = 'source_values' AND jsonb_typeof(value) = 'object' THEN
                    (CASE WHEN jsonb_typeof(results.raw #> '{raw,source_values}') = 'object'
                        THEN results.raw #> '{raw,source_values}' ELSE '{}'::jsonb END) || COALESCE((
                        SELECT jsonb_object_agg(evidence.key, CASE
                            WHEN evidence.key IN ('eventType', 'row', 'context')
                             AND jsonb_typeof(evidence.value) = 'object' THEN
                                (CASE WHEN jsonb_typeof(results.raw #> ARRAY['raw', 'source_values', evidence.key]) = 'object'
                                    THEN results.raw #> ARRAY['raw', 'source_values', evidence.key] ELSE '{}'::jsonb END) || COALESCE((
                                    SELECT jsonb_object_agg(nested.key, CASE
                                        WHEN evidence.key = 'row' AND nested.key = 'attributes'
                                         AND jsonb_typeof(nested.value) = 'object' THEN
                                            (CASE WHEN jsonb_typeof(results.raw #> '{raw,source_values,row,attributes}') = 'object'
                                                THEN results.raw #> '{raw,source_values,row,attributes}' ELSE '{}'::jsonb END) || COALESCE((
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
                    ), '{}'::jsonb)
                ELSE value END)
            FROM jsonb_each(CASE WHEN jsonb_typeof(EXCLUDED.raw->'raw') = 'object'
                THEN EXCLUDED.raw->'raw' ELSE '{}'::jsonb END)
        ), '{}'::jsonb)),
    last_seen = clock_timestamp(),
    normalized_numeric = EXCLUDED.normalized_numeric,
    normalized_unit = EXCLUDED.normalized_unit,
    specification = EXCLUDED.specification
WHERE results.athlete_source_id IS NULL OR EXCLUDED.is_relay
