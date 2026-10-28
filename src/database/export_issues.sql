WITH selected_states AS (
    SELECT lower(btrim(state, U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000')) AS state,
           ordinal
    FROM unnest($1::text[]) WITH ORDINALITY AS selected(state, ordinal)
), scoped_pages AS NOT MATERIALIZED (
    SELECT p.* FROM scrape_pages p
    WHERE cardinality($1::text[]) = 0
       OR EXISTS (
           SELECT 1 FROM selected_states selected
           WHERE selected.state = lower(btrim(COALESCE(p.cursor->'context'->>'acquisition_state', p.cursor->'context'->>'state'),
               U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000'))
       )
       OR (COALESCE(p.cursor->'context'->>'acquisition_state', p.cursor->'context'->>'state') IS NULL AND (
           EXISTS (
               SELECT 1 FROM schools s JOIN selected_states selected
                 ON selected.state = lower(btrim(s.state,
                     U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000'))
               WHERE s.source = p.source
                 AND s.source_id = p.cursor->'context'->>'school_id'
           )
           OR (p.source = 'athleticnet' AND p.cursor->>'kind' = 'directory'
               AND lower(btrim(p.cursor->'context'->>'subject_id')) =
                   (SELECT string_agg(state, ',' ORDER BY ordinal) FROM selected_states))
       ))
), page_status AS MATERIALIZED (
    SELECT source, url, id, status, last_error
    FROM scoped_pages
    WHERE status <> 'succeeded'
      AND (0, id, 0::bigint) > ($2::integer, $3::bigint, $4::bigint)
    ORDER BY id
    LIMIT 1
), note_page AS MATERIALIZED (
    SELECT source, url, id, notes,
           CASE WHEN $2::integer = 1 AND id = $3::bigint
                 AND $4::bigint BETWEEN 0 AND cardinality(notes)::bigint - 1
                THEN $4::bigint + 1 ELSE 1::bigint END AS ordinal
    FROM scoped_pages
    WHERE cardinality(notes) > 0
      AND ($2::integer < 1 OR ($2::integer = 1 AND (
          id > $3::bigint OR (id = $3::bigint AND cardinality(notes)::bigint > $4::bigint)
      )))
    ORDER BY id
    LIMIT 1
), unassigned_result AS MATERIALIZED (
    SELECT r.source, r.source_url AS url, r.id, r.event, r.raw_mark, r.status
    FROM results r
    WHERE r.athlete_id IS NULL
      AND (2, r.id, 0::bigint) > ($2::integer, $3::bigint, $4::bigint)
      AND (
        cardinality($1::text[]) = 0
        OR EXISTS (
            SELECT 1 FROM schools s JOIN selected_states selected
              ON selected.state = lower(btrim(s.state,
                  U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000'))
            WHERE s.id = r.represented_school_id
        )
        OR EXISTS (
            SELECT 1 FROM scoped_pages p
            WHERE p.source = r.source AND p.url = r.source_url
        )
    )
    ORDER BY r.id
    LIMIT 1
), candidates AS (
    SELECT source, url, 0 AS kind, id AS item_id, 0::bigint AS ordinal,
           status, last_error, NULL::text AS note, NULL::text AS event, NULL::text AS raw_mark
    FROM page_status
    UNION ALL
    SELECT source, url, 1, id, ordinal, NULL::text, NULL::text,
           notes[array_lower(notes, 1) + (ordinal - 1)::integer], NULL::text, NULL::text
    FROM note_page
    UNION ALL
    SELECT source, url, 2, id, 0::bigint, status, NULL::text, NULL::text, event, raw_mark
    FROM unassigned_result
), next_issue AS MATERIALIZED (
    SELECT * FROM candidates
    ORDER BY kind, item_id, ordinal
    LIMIT 1
), measured AS (
    SELECT *, octet_length(source)::bigint + octet_length(url)::bigint + CASE kind
        WHEN 0 THEN 5::bigint + octet_length(status)::bigint
            + CASE WHEN last_error IS NULL THEN 0::bigint ELSE 2::bigint + octet_length(last_error)::bigint END
        WHEN 1 THEN 11::bigint + octet_length(note)::bigint
        ELSE 17::bigint
            + CASE WHEN event IS NULL THEN 0::bigint ELSE 3::bigint + octet_length(event)::bigint END
            + CASE WHEN raw_mark IS NULL THEN 0::bigint ELSE 3::bigint + octet_length(raw_mark)::bigint END
            + CASE WHEN status IS NULL THEN 0::bigint ELSE 3::bigint + octet_length(status)::bigint END
        END AS bytes
    FROM next_issue
)
SELECT CASE WHEN bytes <= $5::bigint THEN source ELSE NULL END AS source,
       CASE WHEN bytes <= $5::bigint THEN url ELSE NULL END AS url,
       CASE WHEN bytes <= $5::bigint THEN CASE kind
           WHEN 0 THEN concat_ws(': ', 'page ' || status, last_error)
           WHEN 1 THEN 'page note: ' || note
           ELSE concat_ws(' | ', 'unassigned result', event, raw_mark, status)
       END ELSE NULL END AS detail,
       kind, item_id, ordinal, bytes
FROM measured
