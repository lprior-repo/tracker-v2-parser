WITH cohort AS MATERIALIZED (
    SELECT profile.source,profile.source_id,profile.profile_url,s.source_id AS school_id,
           COALESCE(profile.graduation_year,athlete.graduation_year) AS known_year
    FROM athlete_sources profile JOIN athletes athlete ON athlete.id=profile.athlete_id
    LEFT JOIN schools s ON s.id=profile.school_id AND s.source=profile.source
    WHERE profile.source='milesplit' AND ($1::text IS NULL OR profile.source=$1)
      AND (COALESCE(profile.graduation_year,athlete.graduation_year)=ANY($4::integer[])
        OR COALESCE(profile.graduation_year,athlete.graduation_year) IS NULL)
), profile_candidates AS MATERIALIZED (
    SELECT p.id,p.source,p.url,p.status,p.attempts,c.source_id AS athlete_id,
           COALESCE(NULLIF(p.cursor->'context'->>'school_id',''),c.school_id,'athlete:' || c.source_id) AS school_id,c.profile_url,c.known_year
    FROM scrape_pages p JOIN cohort c
      ON c.source=p.source AND c.source_id=p.cursor->'context'->>'athlete_id'
    WHERE p.source='milesplit' AND p.cursor->>'kind'='athlete'
      AND p.url ~ '^https://(www|[a-z]{2})[.]milesplit[.]com/athletes/[1-9][0-9]*(-[^/?#%]+)?(/(stats|progression))?/?$'
      AND CASE WHEN p.cursor->>'kind'='athlete'
      AND p.cursor->>'source'=p.source AND p.cursor->>'url'=p.url
      AND (p.cursor->>'method' IS NULL OR upper(p.cursor->>'method')='GET')
      AND (p.cursor->'body' IS NULL OR p.cursor->'body'='null'::jsonb)
      AND p.cursor->>'token_url' IS NULL AND p.cursor->>'browser_origin' IS NULL
      AND (cardinality($3::text[])=0
           OR upper(btrim(COALESCE(p.cursor->'context'->>'acquisition_state',p.cursor->'context'->>'state')))=ANY($3)) THEN true ELSE false END
), validated_profiles AS MATERIALIZED (
    SELECT p.id,p.source,p.status,p.attempts,p.athlete_id,
           p.school_id,
           CASE split_part(p.url,'/',6) WHEN '' THEN 0 WHEN 'stats' THEN 1 ELSE 2 END AS section
    FROM profile_candidates p
    WHERE split_part(split_part(p.url,'/',5),'-',1)=p.athlete_id
      AND (split_part(p.url,'/',6)='' OR p.known_year=ANY($4::integer[]))
      AND (split_part(split_part(p.url,'://',2),'.',1)='www'
           OR upper(split_part(split_part(p.url,'://',2),'.',1))=ANY($6::text[]))
      AND (split_part(p.url,'/',6)<>'' OR p.status='succeeded' OR p.url=p.profile_url)
), profiles AS MATERIALIZED (
    SELECT p.*,min(p.id) OVER (PARTITION BY p.source,p.athlete_id) AS athlete_order
    FROM validated_profiles p
), completed AS MATERIALIZED (
    SELECT DISTINCT source,athlete_id,section FROM profiles WHERE status='succeeded'
), parent_candidates AS MATERIALIZED (
    SELECT p.id,p.source,p.url,p.cursor,p.status,p.attempts,p.cursor->'context'->>'school_id' AS school_id
    FROM scrape_pages p
    WHERE p.source='milesplit' AND ($1::text IS NULL OR p.source=$1)
      AND p.cursor->>'kind' IN ('team','roster','contacts')
      AND (cardinality($3::text[])=0
           OR upper(btrim(COALESCE(p.cursor->'context'->>'acquisition_state',p.cursor->'context'->>'state')))=ANY($3))
), known_schools AS MATERIALIZED (
    SELECT DISTINCT source,school_id FROM profiles
    UNION
    SELECT p.source,p.school_id FROM parent_candidates p
    WHERE p.cursor->>'kind' IN ('team','roster')
      AND p.url ~ '^https://(www|[a-z]{2})[.]milesplit[.]com/teams/[1-9][0-9]*(-[^/?#%]+)?(/roster)?/?$'
      AND split_part(split_part(p.url,'/',5),'-',1)=p.school_id
), parent_pages AS MATERIALIZED (
    SELECT p.* FROM parent_candidates p
    WHERE (p.source,p.school_id) IN (SELECT source,school_id FROM known_schools)
), parents AS MATERIALIZED (
    SELECT p.id,p.source,p.url,p.status,p.attempts,p.school_id,
           CASE p.cursor->>'kind' WHEN 'team' THEN 0 WHEN 'roster' THEN 1 ELSE 3 END AS stage
    FROM parent_pages p
    WHERE p.cursor->>'source'=p.source AND p.cursor->>'url'=p.url
      AND (cardinality($3::text[])=0
           OR upper(btrim(COALESCE(p.cursor->'context'->>'acquisition_state',p.cursor->'context'->>'state')))=ANY($3))
      AND CASE WHEN p.cursor->>'kind'='contacts' THEN $7::boolean
        AND NOT (p.url ~ '^https://schools[.]wiaawi[.]org/'
          AND split_part(p.url,'?',1)<>'https://schools.wiaawi.org/Directory/School/GetDirectorySchool')
        AND NOT EXISTS (
          SELECT 1 FROM scrape_pages blocked
          CROSS JOIN LATERAL unnest(ARRAY[blocked.url,blocked.cursor->>'access_stop_url']) AS stop(url)
          WHERE blocked.status='blocked' AND blocked.cursor->>'kind'='contacts'
            AND regexp_replace(lower(substring(stop.url from '(?i)^https?://([^/?#]+)')),
              CASE WHEN lower(stop.url) LIKE 'https://%' THEN ':443$' ELSE ':80$' END,'')=
                regexp_replace(lower(substring(p.url from '(?i)^https?://([^/?#]+)')),
              CASE WHEN lower(p.url) LIKE 'https://%' THEN ':443$' ELSE ':80$' END,''))
        ELSE (p.cursor->>'method' IS NULL OR upper(p.cursor->>'method')='GET')
          AND (p.cursor->'body' IS NULL OR p.cursor->'body'='null'::jsonb)
          AND p.cursor->>'token_url' IS NULL AND p.cursor->>'browser_origin' IS NULL
          AND p.url ~ '^https://(www|[a-z]{2})[.]milesplit[.]com/teams/[1-9][0-9]*(-[^/?#%]+)?(/roster)?/?$'
          AND split_part(split_part(p.url,'/',5),'-',1)=p.school_id
          AND (split_part(split_part(p.url,'://',2),'.',1)='www'
            OR upper(split_part(split_part(p.url,'://',2),'.',1))=ANY($6::text[]))
          AND ((p.cursor->>'kind'='team' AND p.url !~ '/roster/?$')
            OR (p.cursor->>'kind'='roster' AND p.url ~ '/roster/?$')) END
), official_contact_claims AS MATERIALIZED (
    SELECT official.source,official.school_id,official.url,
           NULLIF(btrim(c.name),'') IS NOT NULL AND NULLIF(btrim(c.email),'') IS NOT NULL
             AND NOT COALESCE(c.raw->'review_notes','[]'::jsonb) @> '["Earlier contact source claim; not confirmed by latest parse"]'::jsonb AS valid,
           lower(c.role) ~ '^head[[:space:]-]*coach' AS head,
           lower(c.role) ~ '(^|[^a-z])boys([^a-z]|$)' AS boys,
           lower(c.role) ~ '(^|[^a-z])girls([^a-z]|$)' AS girls,
           c.sport IN ('indoor','outdoor') OR lower(c.role) LIKE '%track%' AS track,
           c.sport='xc' OR lower(c.role) ~ 'cross[[:space:]-]*country|(^|[^a-z])xc([^a-z]|$)' AS xc
    FROM parents official JOIN schools s ON s.source=official.source AND s.source_id=official.school_id
    JOIN contacts c ON c.school_id=s.id AND c.kind='coach' AND c.source_url=official.url
    WHERE official.stage=3 AND official.status='succeeded'
      AND official.url ~ '^https://schools[.]wiaawi[.]org/Directory/School/GetDirectorySchool[?]orgID=[1-9][0-9]{0,9}$'
      AND (c.sport IN ('indoor','outdoor','xc') OR lower(c.role) LIKE '%track%'
        OR lower(c.role) ~ 'cross[[:space:]-]*country|(^|[^a-z])xc([^a-z]|$)')
      AND NULLIF(btrim(s.address),'') IS NOT NULL AND NULLIF(btrim(s.city),'') IS NOT NULL
      AND NULLIF(btrim(s.state),'') IS NOT NULL AND NULLIF(btrim(s.postal_code),'') IS NOT NULL
), official_school_coverage AS MATERIALIZED (
    SELECT DISTINCT source,school_id FROM official_contact_claims
    GROUP BY source,school_id,url
    HAVING bool_and(valid) AND bool_or(valid AND head AND boys AND track)
      AND bool_or(valid AND head AND girls AND track) AND bool_or(valid AND head AND boys AND xc)
      AND bool_or(valid AND head AND girls AND xc)
), school_anchors AS MATERIALIZED (
    SELECT source,school_id,min(id) AS school_order
    FROM (SELECT id,source,school_id FROM profiles UNION ALL SELECT id,source,school_id FROM parents) saved
    GROUP BY source,school_id
), jobs AS (
    SELECT p.id,p.athlete_id,p.attempts,p.source,p.school_id,2 AS stage,p.athlete_order,p.section
    FROM profiles p
    WHERE p.status IN ('pending','failed')
      AND (p.source,p.athlete_id,p.section) NOT IN (SELECT source,athlete_id,section FROM completed)
      AND (p.section=0 OR (p.source,p.athlete_id,0) IN (SELECT source,athlete_id,section FROM completed))
    UNION ALL
    SELECT p.id,NULL::text AS athlete_id,p.attempts,p.source,p.school_id,p.stage,0::bigint,
           CASE WHEN p.stage=3 AND p.url ~ '^https://schools[.]wiaawi[.]org/Directory/School/GetDirectorySchool[?]orgID=[1-9][0-9]{0,9}$' THEN 0 ELSE 1 END
    FROM parents p WHERE p.status IN ('pending','failed')
      AND (p.stage<>3 OR p.attempts<$2 OR $8::text IS NOT NULL)
      AND (p.stage<>3 OR p.url ~ '^https://schools[.]wiaawi[.]org/Directory/School/GetDirectorySchool[?]orgID=[1-9][0-9]{0,9}$'
        OR (p.source,p.school_id) NOT IN (SELECT source,school_id FROM official_school_coverage))
      AND CASE WHEN p.stage=3 THEN
      (p.source,p.school_id,p.url) NOT IN (SELECT source,school_id,url FROM parents WHERE status='succeeded' AND stage=3)
      ELSE (p.source,p.school_id,p.stage) NOT IN (SELECT source,school_id,stage FROM parents WHERE status='succeeded' AND stage<>3) END
), first_job AS (
    SELECT j.id,j.attempts FROM jobs j
    JOIN school_anchors s ON s.source=j.source AND s.school_id=j.school_id
    LEFT JOIN durable_page_operations d ON d.page_id=j.id AND d.run_generation=$8
    WHERE ($8::text IS NULL OR NOT COALESCE(d.state IN ('completed','blocked','rejected','exhausted') OR (d.state='deferred' AND d.retry_at_ms > $9),false))
      AND ($11::boolean OR d.state IS DISTINCT FROM 'deferred')
      AND ($8::text IS NULL OR NOT EXISTS (
        SELECT 1 FROM durable_page_operations active JOIN scrape_pages owned ON owned.id=active.page_id
        WHERE active.state='active' AND owned.id=ANY($10::bigint[]) AND owned.source=j.source
          AND (owned.id=j.id OR (j.stage=2 AND owned.cursor->'context'->>'athlete_id'=j.athlete_id) OR (
            owned.cursor->'context'->>'school_id'=j.school_id
            AND (owned.cursor->>'kind' IN ('team','roster') OR j.stage IN (0,1,3)
              OR (j.stage=2 AND owned.cursor->'context'->>'athlete_id'=j.athlete_id))))))
    ORDER BY COALESCE(d.state='active',false) DESC,COALESCE((d.cycle::bigint-1)*3+d.cycle_attempts,0),s.school_order,j.stage,j.athlete_order,j.section,(j.attempts=0) DESC,j.id
    LIMIT 1
)
SELECT j.id,p.cursor FROM first_job j JOIN scrape_pages p ON p.id=j.id
WHERE $8::text IS NOT NULL OR CASE WHEN $5::boolean THEN j.attempts >= $2 ELSE j.attempts < $2 END
