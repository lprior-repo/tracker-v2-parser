WITH candidates AS MATERIALIZED (
    SELECT p.*, durable.state AS durable_state, durable.retry_at_ms, p.cursor->'context' AS context,
           s.name AS observed_school, a.name AS observed_athlete,
           COALESCE(NULLIF(btrim(p.cursor->'context'->>'acquisition_state'),''),
                    NULLIF(btrim(p.cursor->'context'->>'state'),''),
                    NULLIF(btrim(s.state),''), NULLIF(btrim(athlete_school.state),''),
                    NULLIF(btrim(a.state),''),
                    substring(p.url from '^https?://([A-Za-z]{2})\.milesplit\.com/')) AS scope_state,
           COALESCE(NULLIF(p.cursor->'context'->>'school_id',''),athlete_school.source_id,'') AS school_source_id,
           COALESCE(NULLIF(p.cursor->'context'->>'school_name',''),s.name,athlete_school.name,'') AS school_name
    FROM scrape_pages p
    LEFT JOIN schools s ON s.source=p.source AND s.source_id=p.cursor->'context'->>'school_id'
    LEFT JOIN athlete_sources a ON a.source=p.source AND a.source_id=p.cursor->'context'->>'athlete_id'
    LEFT JOIN schools athlete_school ON athlete_school.id=a.school_id
    LEFT JOIN LATERAL (SELECT state,retry_at_ms FROM durable_page_operations WHERE page_id=p.id ORDER BY created_at DESC,operation_id DESC LIMIT 1) durable ON TRUE
    WHERE p.id>$2::bigint AND p.status IN ('failed','blocked')
), scoped AS MATERIALIZED (
    SELECT * FROM candidates p
    WHERE cardinality($1::text[])=0 OR upper(p.scope_state)=ANY($1::text[])
       OR (p.scope_state IS NULL AND COALESCE(p.context->>'school_id','')=''
           AND COALESCE(p.context->>'athlete_id','')='' AND p.cursor->>'kind' IN ('directory','contacts'))
    ORDER BY id LIMIT $3::bigint
), documents AS (
    SELECT id, jsonb_build_object(
        'checkpoint_id',id,'source',source,'kind',cursor->>'kind','state',COALESCE(upper(scope_state),''),
        'school',school_name,'school_source_id',school_source_id,
        'athlete',COALESCE(observed_athlete,''),'athlete_source_id',COALESCE(context->>'athlete_id',''),
        'url',url,'status',status,'attempts',attempts,'error',COALESCE(last_error,''),
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
