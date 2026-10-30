WITH missing_records AS MATERIALIZED (
    SELECT a.id FROM athletes a
    WHERE a.name IS NOT NULL AND a.graduation_year=ANY($2::integer[])
      AND NOT EXISTS (SELECT 1 FROM results r WHERE r.athlete_id=a.id)
), school_links AS MATERIALIZED (
    SELECT DISTINCT p.athlete_id,p.school_id,
           COALESCE((p.details#>>'{source_values,current_affiliation,school_state_unverified}')::boolean
               AND p.details#>>'{source_values,current_affiliation,unverified_school_state}'=s.state,FALSE) AS state_unverified
    FROM athlete_sources p JOIN missing_records a ON a.id=p.athlete_id
    JOIN schools s ON s.id=p.school_id
), school_state_values AS MATERIALIZED (
    SELECT sl.athlete_id,
           lower(btrim(CASE WHEN sl.state_unverified THEN NULL ELSE s.state END, U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000')) AS state
    FROM school_links sl JOIN schools s ON s.id=sl.school_id
), school_states AS MATERIALIZED (
    SELECT athlete_id,bool_or(NULLIF(state,'') IS NOT NULL) AS has_state,
           bool_or(EXISTS (
               SELECT 1 FROM unnest($1::text[]) selected(state)
               WHERE lower(btrim(selected.state, U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000'))=school_state_values.state
           )) AS matches_state
    FROM school_state_values GROUP BY athlete_id
), selected_athletes AS MATERIALIZED (
    SELECT a.id FROM missing_records a LEFT JOIN school_states ss ON ss.athlete_id=a.id
    WHERE COALESCE(ss.has_state,false) AND COALESCE(ss.matches_state,false)
), unavailable_athletes AS MATERIALIZED (
    SELECT a.id FROM selected_athletes a
    WHERE EXISTS (
        SELECT 1 FROM athlete_sources p WHERE p.athlete_id=a.id AND p.source='milesplit'
          AND jsonb_path_exists(p.details,'$.source_values.pages.*.sections[*] ? (@.attributes.id == "progression" && @.attributes.class like_regex "(^|[[:space:]])progression-cta([[:space:]]|$)")')
        OFFSET 0
    ) AND NOT EXISTS (
        SELECT 1 FROM athlete_sources p WHERE p.athlete_id=a.id
          AND jsonb_path_exists(p.details,'$.source_values.pages.*.pr_rows[*]')
        OFFSET 0
    )
), availability_pages AS MATERIALIZED (
    SELECT min(p.id) AS id,min(school_state.state) AS scope_state FROM scrape_pages p
    JOIN athlete_sources a ON a.source=p.source AND a.source_id=p.cursor->'context'->>'athlete_id'
    JOIN unavailable_athletes unavailable ON unavailable.id=a.athlete_id
    JOIN school_state_values school_state ON school_state.athlete_id=a.athlete_id
    WHERE p.status='succeeded' AND p.source='milesplit' AND p.cursor->>'kind'='athlete'
      AND p.url ~ '^https://(www|[a-z]{2})[.]milesplit[.]com/athletes/[1-9][0-9]*(-[^/?#%]+)?/?$'
      AND split_part(split_part(p.url,'/',5),'-',1)=a.source_id
      AND EXISTS (SELECT 1 FROM unnest($1::text[]) selected(state)
          WHERE lower(btrim(selected.state, U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000'))=school_state.state)
    GROUP BY a.athlete_id
), candidates AS MATERIALIZED (
    SELECT p.*, durable.state AS durable_state, durable.retry_at_ms, p.cursor->'context' AS context,
           availability.id IS NOT NULL AS availability_diagnostic,
           s.name AS observed_school, a.name AS observed_athlete,
           COALESCE(availability.scope_state,NULLIF(btrim(p.cursor->'context'->>'acquisition_state'),''),
                    NULLIF(btrim(p.cursor->'context'->>'state'),''),
                    NULLIF(btrim(s.state),''), NULLIF(btrim(athlete_school.state),''),
                    NULLIF(btrim(a.state),''),
                    substring(p.url from '^https?://([A-Za-z]{2})\.milesplit\.com/')) AS scope_state,
           COALESCE(NULLIF(p.cursor->'context'->>'school_id',''),athlete_school.source_id,'') AS school_source_id,
           COALESCE(NULLIF(p.cursor->'context'->>'school_name',''),s.name,athlete_school.name,'') AS school_name
    FROM scrape_pages p
    LEFT JOIN availability_pages availability ON availability.id=p.id
    LEFT JOIN schools s ON s.source=p.source AND s.source_id=p.cursor->'context'->>'school_id'
    LEFT JOIN athlete_sources a ON a.source=p.source AND a.source_id=p.cursor->'context'->>'athlete_id'
    LEFT JOIN schools athlete_school ON athlete_school.id=a.school_id
    LEFT JOIN LATERAL (SELECT state,retry_at_ms FROM durable_page_operations WHERE page_id=p.id ORDER BY created_at DESC,operation_id DESC LIMIT 1) durable ON TRUE
    WHERE p.id>$3::bigint AND (p.status IN ('failed','blocked') OR availability.id IS NOT NULL)
), scoped AS MATERIALIZED (
    SELECT * FROM candidates p
    WHERE p.availability_diagnostic OR cardinality($1::text[])=0 OR upper(p.scope_state)=ANY($1::text[])
       OR (p.scope_state IS NULL AND COALESCE(p.context->>'school_id','')=''
           AND COALESCE(p.context->>'athlete_id','')='' AND p.cursor->>'kind' IN ('directory','contacts'))
    ORDER BY id LIMIT $4::bigint
), documents AS (
    SELECT id, jsonb_build_object(
        'checkpoint_id',id,'source',source,'kind',cursor->>'kind','state',COALESCE(upper(scope_state),''),
        'school',school_name,'school_source_id',school_source_id,
        'athlete',COALESCE(observed_athlete,''),'athlete_source_id',COALESCE(context->>'athlete_id',''),
        'url',url,'status',status,'attempts',attempts,
        'error',CASE WHEN availability_diagnostic THEN 'No public PR/result rows in captured profile evidence; source progression is restricted. This does not establish career absence.' ELSE COALESCE(last_error,'') END,
        'durable_state',COALESCE(durable_state,''),
        'retry_after',COALESCE(to_char(to_timestamp(retry_at_ms::double precision/1000) AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),''),
        'last_success',COALESCE(to_char(last_success AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),''),
        'created',to_char(created_at AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),
        'next_action','','ai_guidance',''
    ) AS document FROM scoped
)
SELECT CASE WHEN octet_length(document::text)<=65536 THEN document::text END,
       octet_length(document::text)::bigint
FROM documents ORDER BY id
