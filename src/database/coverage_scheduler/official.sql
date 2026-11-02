SELECT s.source_id AS school,c.source_url AS url
FROM schools s JOIN contacts c ON c.school_id=s.id AND c.kind='coach'
WHERE c.source_url=ANY($1::text[]) AND s.source='milesplit'
  AND c.source_url ~ '^https://schools[.]wiaawi[.]org/Directory/School/GetDirectorySchool[?]orgID=[1-9][0-9]{0,9}$'
  AND (c.sport IN ('indoor','outdoor','xc') OR lower(c.role) LIKE '%track%'
    OR lower(c.role) ~ 'cross[[:space:]-]*country|(^|[^a-z])xc([^a-z]|$)')
  AND NULLIF(btrim(s.address),'') IS NOT NULL AND NULLIF(btrim(s.city),'') IS NOT NULL
  AND NULLIF(btrim(s.state),'') IS NOT NULL AND NULLIF(btrim(s.postal_code),'') IS NOT NULL
GROUP BY s.source_id,c.source_url
HAVING bool_and(NULLIF(btrim(c.name),'') IS NOT NULL AND NULLIF(btrim(c.email),'') IS NOT NULL
       AND NOT COALESCE(c.raw->'review_notes','[]'::jsonb) @> '["Earlier contact source claim; not confirmed by latest parse"]'::jsonb)
   AND bool_or(lower(c.role) ~ '^head[[:space:]-]*coach' AND lower(c.role) ~ '(^|[^a-z])boys([^a-z]|$)' AND (c.sport IN ('indoor','outdoor') OR lower(c.role) LIKE '%track%'))
   AND bool_or(lower(c.role) ~ '^head[[:space:]-]*coach' AND lower(c.role) ~ '(^|[^a-z])girls([^a-z]|$)' AND (c.sport IN ('indoor','outdoor') OR lower(c.role) LIKE '%track%'))
   AND bool_or(lower(c.role) ~ '^head[[:space:]-]*coach' AND lower(c.role) ~ '(^|[^a-z])boys([^a-z]|$)' AND (c.sport='xc' OR lower(c.role) ~ 'cross[[:space:]-]*country|(^|[^a-z])xc([^a-z]|$)'))
   AND bool_or(lower(c.role) ~ '^head[[:space:]-]*coach' AND lower(c.role) ~ '(^|[^a-z])girls([^a-z]|$)' AND (c.sport='xc' OR lower(c.role) ~ 'cross[[:space:]-]*country|(^|[^a-z])xc([^a-z]|$)'))
LIMIT 250001
