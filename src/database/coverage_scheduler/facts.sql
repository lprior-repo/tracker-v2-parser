SELECT p.id,p.status,p.attempts,p.url,profile.id AS profile_id,profile.athlete_id AS athlete_id,profile.school_id AS school_id,
       CASE p.cursor->>'kind' WHEN 'team' THEN 0 WHEN 'roster' THEN 1 WHEN 'athlete' THEN 2 ELSE 3 END::smallint AS stage,
       CASE split_part(p.url,'/',6) WHEN '' THEN 0 WHEN 'stats' THEN 1 ELSE 2 END::smallint AS section,
       CASE WHEN p.cursor->>'kind'='athlete' THEN profile.source_id END AS athlete,
       CASE WHEN p.cursor->>'kind'='athlete' THEN
         COALESCE(NULLIF(p.cursor->'context'->>'school_id',''),s.source_id,'athlete:' || profile.source_id)
         ELSE p.cursor->'context'->>'school_id' END AS school,
       COALESCE(p.cursor->>'source'=p.source AND p.cursor->>'url'=p.url
       AND CASE WHEN p.cursor->>'kind'='contacts' THEN
         NOT (p.url ~ '^https://schools[.]wiaawi[.]org/'
           AND split_part(p.url,'?',1)<>'https://schools.wiaawi.org/Directory/School/GetDirectorySchool')
       ELSE (p.cursor->>'method' IS NULL OR upper(p.cursor->>'method')='GET')
         AND (p.cursor->'body' IS NULL OR p.cursor->'body'='null'::jsonb)
         AND p.cursor->>'token_url' IS NULL AND p.cursor->>'browser_origin' IS NULL
         AND (split_part(split_part(p.url,'://',2),'.',1)='www'
           OR upper(split_part(split_part(p.url,'://',2),'.',1))=ANY($3::text[]))
         AND CASE WHEN p.cursor->>'kind'='athlete' THEN
           p.url ~ '^https://(www|[a-z]{2})[.]milesplit[.]com/athletes/[1-9][0-9]*(-[^/?#%]+)?(/(stats|progression))?/?$'
           AND split_part(split_part(p.url,'/',5),'-',1)=profile.source_id
           AND (split_part(p.url,'/',6)='' OR COALESCE(profile.graduation_year,a.graduation_year)=ANY($2::integer[]))
           AND (split_part(p.url,'/',6)<>'' OR p.status='succeeded' OR p.url=profile.profile_url)
         ELSE p.url ~ '^https://(www|[a-z]{2})[.]milesplit[.]com/teams/[1-9][0-9]*(-[^/?#%]+)?(/roster)?/?$'
           AND split_part(split_part(p.url,'/',5),'-',1)=p.cursor->'context'->>'school_id'
           AND ((p.cursor->>'kind'='team' AND p.url !~ '/roster/?$')
             OR (p.cursor->>'kind'='roster' AND p.url ~ '/roster/?$')) END END,false) AS valid,
       COALESCE(p.cursor->>'kind' IN ('team','roster')
         AND p.url ~ '^https://(www|[a-z]{2})[.]milesplit[.]com/teams/[1-9][0-9]*(-[^/?#%]+)?(/roster)?/?$'
         AND split_part(split_part(p.url,'/',5),'-',1)=p.cursor->'context'->>'school_id',false) AS known_school,
       p.url ~ '^https://schools[.]wiaawi[.]org/Directory/School/GetDirectorySchool[?]orgID=[1-9][0-9]{0,9}$' AS official,
       regexp_replace(lower(substring(p.url from '(?i)^https?://([^/?#]+)')),
         CASE WHEN lower(p.url) LIKE 'https://%' THEN ':443$' ELSE ':80$' END,'') AS authority
FROM scrape_pages p
LEFT JOIN athlete_sources profile ON p.cursor->>'kind'='athlete' AND profile.source=p.source
    AND profile.source_id=p.cursor->'context'->>'athlete_id'
LEFT JOIN athletes a ON a.id=profile.athlete_id
LEFT JOIN schools s ON s.id=profile.school_id AND s.source=profile.source
WHERE p.source='milesplit' AND p.cursor->>'kind' IN ('athlete','team','roster','contacts')
  AND (cardinality($1::text[])=0 OR (
       left(upper(btrim(COALESCE(p.cursor->'context'->>'acquisition_state',p.cursor->'context'->>'state'))),2)
         =ANY(ARRAY(SELECT left(state,2) FROM unnest($1::text[]) state))
       AND upper(btrim(COALESCE(p.cursor->'context'->>'acquisition_state',p.cursor->'context'->>'state')))=ANY($1)))
  AND (p.cursor->>'kind'<>'athlete' OR (profile.id IS NOT NULL
       AND (COALESCE(profile.graduation_year,a.graduation_year)=ANY($2::integer[])
         OR COALESCE(profile.graduation_year,a.graduation_year) IS NULL)))
