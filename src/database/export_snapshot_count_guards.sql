SELECT candidate.id, guard.kind, guard.row_count
FROM unnest($1::bigint[]) candidate(id)
CROSS JOIN LATERAL (
    SELECT 'athlete observations'::text AS kind, count(*)::bigint AS row_count
    FROM (
        SELECT id
        FROM (
            SELECT r.id
            FROM results r
            JOIN athlete_sources p ON p.id = r.athlete_source_id
            WHERE p.athlete_id = candidate.id
            UNION
            SELECT r.id
            FROM valid_reviewed_result_links l
            JOIN results r ON r.id = l.result_id
            WHERE l.target_athlete_id = candidate.id
        ) observed
        LIMIT 10001
    ) bounded_observations
    UNION ALL
    SELECT 'athlete profiles', count(*)::bigint
    FROM (
        SELECT id
        FROM athlete_sources
        WHERE athlete_id = candidate.id
        LIMIT 10001
    ) bounded_profiles
) guard
ORDER BY candidate.id,
         CASE guard.kind
             WHEN 'athlete observations' THEN 0
             ELSE 1
         END
