WITH completed_parents AS MATERIALIZED (
    SELECT DISTINCT source,cursor->>'kind' kind,cursor->'context'->>'school_id' school_id
    FROM scrape_pages
    WHERE source='milesplit' AND status='succeeded'
      AND cursor->>'source'=source AND cursor->>'url'=url
      AND split_part(split_part(url,'/',5),'-',1)=cursor->'context'->>'school_id'
      AND ((cursor->>'kind'='team'
        AND url ~ '^https://(www|[a-z]{2})[.]milesplit[.]com/teams/[1-9][0-9]*(-[^/?#%]+)?/?$')
        OR (cursor->>'kind'='roster'
        AND url ~ '^https://(www|[a-z]{2})[.]milesplit[.]com/teams/[1-9][0-9]*(-[^/?#%]+)?/roster/?$'))
)
SELECT p.id,p.cursor FROM scrape_pages p WHERE p.status IN ('pending','failed')
 AND ($6::text IS NOT NULL OR p.attempts < $2)
 AND ($1::text IS NULL OR p.source=$1)
 AND NOT (p.cursor->>'kind'='contacts'
 AND p.url ~ '^https://schools[.]wiaawi[.]org/'
 AND split_part(p.url,'?',1) <> 'https://schools.wiaawi.org/Directory/School/GetDirectorySchool')
 AND (p.cursor->>'kind' <> 'athlete' OR cardinality($5::integer[]) = 0 OR NOT EXISTS ( SELECT 1 FROM athlete_sources profile JOIN athletes athlete ON athlete.id=profile.athlete_id WHERE profile.source=p.source
 AND profile.source_id=p.cursor->'context'->>'athlete_id'
 AND COALESCE(profile.graduation_year,athlete.graduation_year) IS NOT NULL
 AND NOT (COALESCE(profile.graduation_year,athlete.graduation_year)=ANY($5))))
 AND (cardinality($4::text[]) = 0 OR upper(btrim(COALESCE(p.cursor->'context'->>'acquisition_state',p.cursor->'context'->>'state')))=ANY($4) OR (p.source='athleticnet'
 AND p.cursor->>'kind'='directory'
 AND p.cursor->'context'->>'state' IS NULL
 AND p.cursor->'context'->>'subject_id'=array_to_string($4,',')))
 AND (p.cursor->>'kind' <> 'contacts' OR NOT EXISTS ( SELECT 1 FROM scrape_pages blocked CROSS JOIN LATERAL unnest(ARRAY[blocked.url,blocked.cursor->>'access_stop_url']) AS stop(url) WHERE blocked.status='blocked'
 AND blocked.cursor->>'kind'='contacts'
 AND regexp_replace(lower(substring(stop.url from '(?i)^https?://([^/?#]+)')), CASE WHEN lower(stop.url) LIKE 'https://%' THEN ':443$' ELSE ':80$' END,'') = regexp_replace(lower(substring(p.url from '(?i)^https?://([^/?#]+)')), CASE WHEN lower(p.url) LIKE 'https://%' THEN ':443$' ELSE ':80$' END,'')))
 AND ($6::text IS NULL OR NOT EXISTS (SELECT 1 FROM durable_page_operations d WHERE d.page_id=p.id
 AND d.run_generation=$6
 AND (d.state IN ('completed','blocked','rejected','exhausted') OR (d.state='deferred'
 AND d.retry_at_ms > $7))))
 AND ($6::text IS NULL OR NOT EXISTS (SELECT 1 FROM durable_page_operations active JOIN scrape_pages owned ON owned.id=active.page_id WHERE active.state='active'
 AND owned.id=ANY($8::bigint[])
 AND owned.source=p.source
 AND (owned.id=p.id OR (owned.cursor->'context'->>'athlete_id' IS NOT NULL
 AND owned.cursor->'context'->>'athlete_id'=p.cursor->'context'->>'athlete_id') OR (owned.cursor->'context'->>'school_id'=p.cursor->'context'->>'school_id'
 AND (owned.cursor->>'kind' IN ('team','roster') OR p.cursor->>'kind' IN ('team','roster','contacts'))))))
 AND (NOT $9::boolean OR p.source<>'milesplit' OR CASE p.cursor->>'kind'
 WHEN 'contacts' THEN false
 WHEN 'athlete' THEN
   p.cursor->>'source'=p.source
 AND p.cursor->>'url'=p.url
 AND (p.cursor->>'method' IS NULL OR upper(p.cursor->>'method')='GET')
 AND (p.cursor->'body' IS NULL OR p.cursor->'body'='null'::jsonb)
 AND p.cursor->>'token_url' IS NULL
 AND p.cursor->>'browser_origin' IS NULL
 AND p.url ~ '^https://(www|[a-z]{2})[.]milesplit[.]com/athletes/[1-9][0-9]*(-[^/?#%]+)?/?$'
 AND (split_part(split_part(p.url,'://',2),'.',1)='www'
     OR upper(split_part(split_part(p.url,'://',2),'.',1))=ANY($10::text[]))
   AND split_part(split_part(p.url,'/',5),'-',1)=p.cursor->'context'->>'athlete_id'
 AND NOT EXISTS(SELECT 1 FROM athlete_sources registered
     WHERE registered.source=p.source
 AND registered.source_id=p.cursor->'context'->>'athlete_id')
 WHEN 'team' THEN NOT (
   p.url ~ '^https://(www|[a-z]{2})[.]milesplit[.]com/teams/[1-9][0-9]*(-[^/?#%]+)?/?$'
   AND split_part(split_part(p.url,'/',5),'-',1)=p.cursor->'context'->>'school_id'
   AND EXISTS(SELECT 1 FROM completed_parents done WHERE done.source=p.source
     AND done.kind='team' AND done.school_id=p.cursor->'context'->>'school_id'))
 WHEN 'roster' THEN NOT (
   p.url ~ '^https://(www|[a-z]{2})[.]milesplit[.]com/teams/[1-9][0-9]*(-[^/?#%]+)?/roster/?$'
   AND split_part(split_part(p.url,'/',5),'-',1)=p.cursor->'context'->>'school_id'
   AND EXISTS(SELECT 1 FROM completed_parents done WHERE done.source=p.source
     AND done.kind='roster' AND done.school_id=p.cursor->'context'->>'school_id'))
 ELSE true END)
ORDER BY EXISTS (SELECT 1 FROM durable_page_operations d WHERE d.page_id=p.id
 AND d.run_generation=$6
 AND d.state='active') DESC, (p.attempts = 0) DESC, CASE p.cursor->>'kind' WHEN 'athlete' THEN ($3 <> 'profile')::int WHEN 'roster' THEN ($3 <> 'roster')::int WHEN 'contacts' THEN ($3 <> 'contacts')::int ELSE ($3 <> 'bulk')::int END, CASE p.cursor->>'kind' WHEN 'athlete' THEN 0 WHEN 'roster' THEN 1 WHEN 'contacts' THEN 2 ELSE 3 END, CASE WHEN p.cursor->>'kind'='contacts' THEN CASE p.cursor->'context'->>'subject_id' WHEN 'contacts-depth:2' THEN 0 WHEN 'contacts-depth:1' THEN 1 ELSE 2 END ELSE 0 END, p.id LIMIT 1
