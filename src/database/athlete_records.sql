WITH school_links AS (
    SELECT DISTINCT p.athlete_id, p.school_id,
           COALESCE((p.details#>>'{source_values,current_affiliation,school_state_unverified}')::boolean
               AND p.details#>>'{source_values,current_affiliation,unverified_school_state}'=s.state,FALSE) AS state_unverified
    FROM athlete_sources p JOIN schools s ON s.id=p.school_id
    WHERE p.school_id IS NOT NULL
), school_state_values AS (
    SELECT sl.athlete_id,
           lower(btrim(CASE WHEN sl.state_unverified THEN NULL ELSE s.state END, U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000')) AS state
    FROM school_links sl JOIN schools s ON s.id = sl.school_id
), school_states AS (
    SELECT athlete_id,
           bool_or(NULLIF(state, '') IS NOT NULL) AS has_state,
           bool_or(EXISTS (
               SELECT 1 FROM unnest($1::text[]) AS selected(state)
               WHERE lower(btrim(selected.state, U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000')) = school_state_values.state
           )) AS matches_state
    FROM school_state_values
    GROUP BY athlete_id
), selected AS (
    SELECT a.*,
           a.needs_review OR EXISTS (SELECT 1 FROM school_links sl WHERE sl.athlete_id=a.id AND sl.state_unverified)
               OR NOT EXISTS (SELECT 1 FROM school_links sl WHERE sl.athlete_id = a.id)
               OR (NULLIF(btrim(a.state, U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000'), '') IS NULL AND NOT COALESCE(ss.has_state, false))
               OR a.graduation_year IS NULL AS report_needs_review
    FROM athletes a
    LEFT JOIN school_states ss ON ss.athlete_id = a.id
    WHERE a.name IS NOT NULL
      AND (cardinality($1::text[]) = 0 OR CASE WHEN COALESCE(ss.has_state, false)
           THEN COALESCE(ss.matches_state, false)
           ELSE NULLIF(btrim(a.state, U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000'), '') IS NULL
                OR EXISTS (
                    SELECT 1 FROM unnest($1::text[]) selected(state)
                    WHERE lower(btrim(selected.state, U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000')) =
                          lower(btrim(a.state, U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000'))
                ) END)
      AND (cardinality($2::integer[]) = 0 OR a.graduation_year = ANY($2) OR a.graduation_year IS NULL)
), profile_data AS (
    SELECT p.athlete_id,
           COALESCE(jsonb_agg(DISTINCT p.profile_url ORDER BY p.profile_url)
                    FILTER (WHERE p.profile_url IS NOT NULL), '[]'::jsonb) AS profiles,
           COALESCE(jsonb_agg(jsonb_build_object(
               'source', p.source, 'source_id', p.source_id, 'url', p.profile_url,
               'source_values', p.details->'source_values'
           ) ORDER BY p.source, p.source_id)
           FILTER (WHERE jsonb_typeof(p.details->'source_values') = 'object'), '[]'::jsonb)
               AS profile_details
    FROM athlete_sources p JOIN selected a ON a.id = p.athlete_id
    GROUP BY p.athlete_id
), school_data AS (
    SELECT sl.athlete_id,
           jsonb_agg(jsonb_build_object(
               'id', s.source_id, 'uuid', s.uuid, 'name', s.name,
               'state', CASE WHEN sl.state_unverified THEN NULL ELSE s.state END, 'city', s.city,
               'address', s.address, 'postal_code', s.postal_code, 'website', s.website,
               'public_email', s.public_email
           ) ORDER BY s.source, s.source_id) AS schools
    FROM school_links sl JOIN schools s ON s.id = sl.school_id
    JOIN selected a ON a.id = sl.athlete_id
    GROUP BY sl.athlete_id
), coach_data AS (
    SELECT sl.athlete_id,
           jsonb_agg(jsonb_build_object(
               'school_id', s.source_id, 'name', c.name, 'role', c.role, 'email', c.email,
               'sport', CASE WHEN c.sport = 'xc' THEN 'cross_country' ELSE c.sport END,
               'review_notes', COALESCE(c.raw->'review_notes', '[]'::jsonb)
           ) ORDER BY s.source, s.source_id, c.id) AS coaches
    FROM school_links sl JOIN schools s ON s.id = sl.school_id
    JOIN selected a ON a.id = sl.athlete_id
    JOIN contacts c ON c.school_id = s.id AND c.kind = 'coach'
    GROUP BY sl.athlete_id
), performance_data AS (
    SELECT owner.athlete_id,
           jsonb_agg(jsonb_build_object(
               'source', r.source,
               'url', r.source_url,
               'performance', CASE WHEN owner.reviewed_link IS NULL THEN r.raw ELSE
                   jsonb_build_object(
                       'athlete_id', owner.source_athlete_id,
                       'event', r.event,
                       'mark', r.raw_mark,
                       'reported_status', NULL,
                       'source_values', COALESCE(r.raw->'raw','{}'::jsonb)
                           || jsonb_build_object('reviewed_result_link', jsonb_build_object(
                               'reviewed_by', owner.reviewed_by,
                               'researcher', owner.researcher,
                               'reviewed_at', owner.reviewed_at,
                               'manifest_sha256', owner.manifest_sha256,
                               'evidence', owner.evidence
                           )),
                       'sport', CASE WHEN r.sport='xc' THEN 'cross_country' ELSE r.sport END,
                       'result_id', r.source_result_id,
                       'source_row', r.raw->>'source_row',
                       'school_id', COALESCE(represented_school.source_id,r.raw->>'school_id'),
                       'meet_id', r.meet_id,
                       'meet_name', COALESCE(r.meet_name,r.raw->'raw'->>'meet_name'),
                       'date', COALESCE(r.meet_date,r.raw->>'date'),
                       'wind', r.wind,
                       'timing', r.timing,
                       'course', r.course,
                       'is_personal_record', r.is_personal_record,
                       'is_relay', r.is_relay,
                       'season', r.season,
                       'grade', r.grade,
                       'gender', r.gender
                   ) END
           ) ORDER BY r.source, r.id) AS performances
    FROM (
        SELECT r.id,p.athlete_id,p.source_id AS source_athlete_id,
               NULL::bigint AS reviewed_link,NULL::text AS reviewed_by,
               NULL::text AS researcher,NULL::timestamptz AS reviewed_at,
               NULL::text AS manifest_sha256,NULL::jsonb AS evidence
        FROM results r JOIN athlete_sources p ON p.id=r.athlete_source_id
        JOIN selected a ON a.id=p.athlete_id
        UNION ALL
        SELECT r.id,l.target_athlete_id,p.source_id,l.result_id,l.reviewed_by,
               l.researcher,l.reviewed_at,l.manifest_sha256,l.evidence
        FROM valid_reviewed_result_links l
        JOIN results r ON r.id=l.result_id
        JOIN athlete_sources p ON p.id=l.target_athlete_source_id
        JOIN selected a ON a.id=l.target_athlete_id
    ) owner
    JOIN results r ON r.id=owner.id
    LEFT JOIN schools represented_school ON represented_school.id=r.represented_school_id
    JOIN selected a ON a.id = owner.athlete_id
    GROUP BY owner.athlete_id
)
SELECT jsonb_build_object(
    'id', a.id, 'name', a.name, 'gender', a.gender, 'grade', a.grade,
    'graduation_year', a.graduation_year, 'needs_review', a.report_needs_review,
    'schools', COALESCE(sd.schools, '[]'::jsonb),
    'coaches', COALESCE(cd.coaches, '[]'::jsonb),
    'profiles', COALESCE(pd.profiles, '[]'::jsonb),
    'profile_details', COALESCE(pd.profile_details, '[]'::jsonb),
    'performances', COALESCE(pr.performances, '[]'::jsonb)
)
FROM selected a
LEFT JOIN profile_data pd ON pd.athlete_id = a.id
LEFT JOIN school_data sd ON sd.athlete_id = a.id
LEFT JOIN coach_data cd ON cd.athlete_id = a.id
LEFT JOIN performance_data pr ON pr.athlete_id = a.id
ORDER BY a.id
