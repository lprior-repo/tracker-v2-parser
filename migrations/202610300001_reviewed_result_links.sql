CREATE TABLE reviewed_result_links (
    source_result_key TEXT PRIMARY KEY REFERENCES results(source_result_key) ON DELETE RESTRICT,
    target_athlete_source_id BIGINT NOT NULL REFERENCES athlete_sources(id) ON DELETE RESTRICT,
    reviewed_by TEXT NOT NULL CHECK (reviewed_by = 'codex'),
    researcher TEXT NOT NULL CHECK (researcher = 'gpt-6-luna'),
    reviewed_at TIMESTAMPTZ NOT NULL,
    authorization_quote TEXT NOT NULL CHECK (btrim(authorization_quote) <> ''),
    manifest_sha256 TEXT NOT NULL CHECK (manifest_sha256 ~ '^[0-9a-f]{64}$'),
    evidence JSONB NOT NULL CHECK (jsonb_typeof(evidence) = 'array' AND jsonb_array_length(evidence) > 0),
    result_snapshot JSONB NOT NULL CHECK (jsonb_typeof(result_snapshot) = 'object'),
    target_snapshot JSONB NOT NULL CHECK (jsonb_typeof(target_snapshot) = 'object'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CHECK (result_snapshot->>'source_result_key' = source_result_key),
    CHECK (target_snapshot->>'athlete_source_id' = target_athlete_source_id::text)
);

CREATE VIEW reviewed_result_link_state AS
SELECT link.source_result_key, link.target_athlete_source_id, link.reviewed_by,
       link.researcher, link.reviewed_at, link.manifest_sha256, link.evidence,
       result.id AS result_id, target.athlete_id AS target_athlete_id,
       CASE
           WHEN result.athlete_source_id IS NOT NULL OR result.athlete_id IS NOT NULL
               THEN 'physical_owner_present'
           WHEN link.result_snapshot <> jsonb_build_object(
               'result_id',result.id,
               'source_result_key',result.source_result_key,
               'source_result_id',result.source_result_id,
               'source',result.source,
               'athlete_source_id',result.athlete_source_id,
               'athlete_id',result.athlete_id,
               'represented_school_id',result.represented_school_id,
               'event',result.event,
               'raw_mark',result.raw_mark,
               'sport',result.sport,
               'meet_id',result.meet_id,
               'meet_name',result.meet_name,
               'meet_date',result.meet_date,
               'wind',result.wind,
               'timing',result.timing,
               'specification',result.specification,
               'course',result.course,
               'status',result.status,
               'is_personal_record',result.is_personal_record,
               'is_relay',result.is_relay,
               'season',result.season,
               'grade',result.grade,
               'gender',result.gender,
               'source_url',result.source_url,
               'raw',result.raw,
               'normalized_numeric',result.normalized_numeric,
               'normalized_unit',result.normalized_unit
           ) THEN 'result_snapshot_changed'
           WHEN link.target_snapshot <> jsonb_build_object(
               'athlete_source_id',target.id,
               'source',target.source,
               'source_id',target.source_id,
               'profile_url',target.profile_url,
               'athlete_id',target.athlete_id,
               'profile_name',target.name,
               'profile_school_id',target.school_id,
               'profile_gender',target.gender,
               'profile_grade',target.grade,
               'profile_graduation_year',target.graduation_year,
               'athlete_name',athlete.name,
               'athlete_gender',athlete.gender,
               'athlete_graduation_year',athlete.graduation_year,
               'athlete_school_id',athlete.school_id,
               'athlete_state',athlete.state,
               'school_source',school.source,
               'school_source_id',school.source_id,
               'school_name',school.name,
               'school_state',school.state
           ) THEN 'target_snapshot_changed'
           ELSE NULL
       END AS invalidation_reason
FROM reviewed_result_links link
JOIN results result ON result.source_result_key = link.source_result_key
JOIN athlete_sources target ON target.id = link.target_athlete_source_id
JOIN athletes athlete ON athlete.id = target.athlete_id
LEFT JOIN schools school ON school.id = target.school_id;

CREATE VIEW valid_reviewed_result_links AS
SELECT * FROM reviewed_result_link_state WHERE invalidation_reason IS NULL;

CREATE VIEW invalid_reviewed_result_links AS
SELECT * FROM reviewed_result_link_state WHERE invalidation_reason IS NOT NULL;
