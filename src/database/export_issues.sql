WITH scoped_pages AS (
    SELECT p.* FROM scrape_pages p
    WHERE cardinality($1::text[]) = 0
       OR p.cursor->'context'->>'state' = ANY($1)
       OR (p.cursor->'context'->>'state' IS NULL AND (
           EXISTS (
               SELECT 1 FROM schools s
               WHERE s.source = p.source
                 AND s.source_id = p.cursor->'context'->>'school_id'
                 AND s.state = ANY($1)
           )
           OR (p.source = 'athleticnet' AND p.cursor->>'kind' = 'directory'
               AND p.cursor->'context'->>'subject_id' = array_to_string($1, ','))
       ))
)
SELECT source, url, detail FROM (
    SELECT source, url, concat_ws(': ', 'page ' || status, last_error) AS detail
    FROM scoped_pages WHERE status <> 'succeeded'
    UNION ALL
    SELECT source, url, 'page note: ' || note
    FROM scoped_pages CROSS JOIN LATERAL unnest(notes) AS note
    UNION ALL
    SELECT r.source, r.source_url,
           concat_ws(' | ', 'unassigned result', r.event, r.raw_mark, r.status)
    FROM results r
    WHERE r.athlete_id IS NULL AND (
        cardinality($1::text[]) = 0
        OR EXISTS (
            SELECT 1 FROM schools s
            WHERE s.id = r.represented_school_id AND s.state = ANY($1)
        )
        OR EXISTS (
            SELECT 1 FROM scoped_pages p
            WHERE p.source = r.source AND p.url = r.source_url
        )
    )
) issues ORDER BY source, url, detail
