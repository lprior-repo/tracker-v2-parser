WITH school_ids AS MATERIALIZED (
    SELECT DISTINCT school_id AS id FROM athlete_sources WHERE athlete_id=$1 AND school_id IS NOT NULL
), bounded_profiles AS MATERIALIZED (
    SELECT id FROM athlete_sources WHERE athlete_id=$1 ORDER BY id LIMIT 10001
), bounded_results AS MATERIALIZED (
    SELECT r.id FROM results r JOIN athlete_sources p ON p.id=r.athlete_source_id
    WHERE p.athlete_id=$1 ORDER BY r.id LIMIT 10001
), bounded_schools AS MATERIALIZED (
    SELECT s.id FROM schools s JOIN school_ids ids ON ids.id=s.id ORDER BY s.id LIMIT 10001
), bounded_contacts AS MATERIALIZED (
    SELECT c.id FROM contacts c JOIN school_ids ids ON ids.id=c.school_id
    WHERE c.kind='coach' ORDER BY c.id LIMIT 10001
), components AS (
    SELECT 'athlete fields' AS kind, 1024::bigint
        + COALESCE(octet_length(to_jsonb(a.name)::text)::bigint,0)
        + COALESCE(octet_length(to_jsonb(a.gender)::text)::bigint,0)
        + COALESCE(octet_length(to_jsonb(a.grade)::text)::bigint,0) AS bytes
    FROM athletes a WHERE a.id=$1
    UNION ALL
    SELECT 'athlete profiles', 1024::bigint
        + COALESCE(octet_length(to_jsonb(p.source)::text)::bigint,0)
        + COALESCE(octet_length(to_jsonb(p.source_id)::text)::bigint,0)
        + 2 * COALESCE(octet_length(to_jsonb(p.profile_url)::text)::bigint,0)
        + COALESCE(octet_length((p.details->'source_values')::text)::bigint,0)
    FROM athlete_sources p JOIN bounded_profiles ids ON ids.id=p.id
    UNION ALL
    SELECT 'athlete observations', 1024::bigint
        + COALESCE(octet_length(to_jsonb(r.source)::text)::bigint,0)
        + COALESCE(octet_length(to_jsonb(r.source_url)::text)::bigint,0)
        + COALESCE(octet_length(r.raw::text)::bigint,0)
    FROM results r JOIN bounded_results ids ON ids.id=r.id
    UNION ALL
    SELECT 'athlete schools', 1024::bigint
        + COALESCE(octet_length(to_jsonb(s.source_id)::text)::bigint,0)
        + COALESCE(octet_length(to_jsonb(s.uuid)::text)::bigint,0)
        + COALESCE(octet_length(to_jsonb(s.name)::text)::bigint,0)
        + COALESCE(octet_length(to_jsonb(s.state)::text)::bigint,0)
        + COALESCE(octet_length(to_jsonb(s.city)::text)::bigint,0)
        + COALESCE(octet_length(to_jsonb(s.address)::text)::bigint,0)
        + COALESCE(octet_length(to_jsonb(s.postal_code)::text)::bigint,0)
        + COALESCE(octet_length(to_jsonb(s.website)::text)::bigint,0)
        + COALESCE(octet_length(to_jsonb(s.public_email)::text)::bigint,0)
    FROM schools s JOIN bounded_schools ids ON ids.id=s.id
    UNION ALL
    SELECT 'athlete coaches', 1024::bigint
        + COALESCE(octet_length(to_jsonb(s.source_id)::text)::bigint,0)
        + COALESCE(octet_length(to_jsonb(c.name)::text)::bigint,0)
        + COALESCE(octet_length(to_jsonb(c.role)::text)::bigint,0)
        + COALESCE(octet_length(to_jsonb(c.email)::text)::bigint,0)
        + COALESCE(octet_length(to_jsonb(c.sport)::text)::bigint,0)
        + COALESCE(octet_length((c.raw->'review_notes')::text)::bigint,0)
    FROM contacts c JOIN bounded_contacts ids ON ids.id=c.id JOIN schools s ON s.id=c.school_id
)
SELECT kind, count(*)::bigint, sum(bytes)::bigint FROM components GROUP BY kind
