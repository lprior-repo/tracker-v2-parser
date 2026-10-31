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
    SELECT id FROM scoped_pages
    WHERE $2::integer=0 AND id>$3::bigint AND status<>'succeeded'
    ORDER BY id LIMIT $6::bigint
), note_pages AS MATERIALIZED (
    SELECT id, cardinality(notes)::bigint AS total,
           CASE WHEN id=$3::bigint THEN $4::bigint+1 ELSE 1::bigint END AS first
    FROM scoped_pages
    WHERE $2::integer=1 AND id>=$3::bigint AND cardinality(notes)>0
      AND (id>$3::bigint OR (id=$3::bigint AND cardinality(notes)::bigint>$4::bigint))
    ORDER BY id LIMIT $6::bigint
), note_budgets AS (
    SELECT *, COALESCE(sum(total-first+1) OVER (
        ORDER BY id ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING
    ),0)::bigint AS preceding
    FROM note_pages
), note_tokens AS MATERIALIZED (
    SELECT p.id, ordinal
    FROM note_budgets p
    CROSS JOIN LATERAL generate_series(p.first,least(p.total,p.first+$6::bigint-p.preceding-1)) ordinal
    WHERE p.preceding<$6::bigint
), unassigned_results AS MATERIALIZED (
    SELECT r.id FROM results r
    WHERE $2::integer=2 AND r.id>$3::bigint AND r.athlete_id IS NULL
      AND NOT EXISTS (SELECT 1 FROM reviewed_result_links l WHERE l.source_result_key=r.source_result_key)
      AND (
        cardinality($1::text[])=0
        OR EXISTS (
            SELECT 1 FROM schools s JOIN selected_states selected
              ON selected.state=lower(btrim(s.state,
                  U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000'))
            WHERE s.id=r.represented_school_id
        )
        OR EXISTS (
            SELECT 1 FROM scoped_pages p WHERE p.source=r.source AND p.url=r.source_url
        )
      )
    ORDER BY r.id LIMIT $6::bigint
), invalid_reviewed_results AS MATERIALIZED (
    SELECT r.id FROM invalid_reviewed_result_links l JOIN results r ON r.id=l.result_id
    WHERE $2::integer=3 AND r.id>$3::bigint
      AND (
        cardinality($1::text[])=0
        OR EXISTS (
            SELECT 1 FROM schools s JOIN selected_states selected
              ON selected.state=lower(btrim(s.state,
                  U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000'))
            WHERE s.id=(SELECT p.school_id FROM athlete_sources p WHERE p.id=l.target_athlete_source_id)
        )
        OR EXISTS (
            SELECT 1 FROM athletes a JOIN selected_states selected
              ON selected.state=lower(btrim(a.state,
                  U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000'))
            WHERE a.id=l.target_athlete_id
        )
      )
    ORDER BY r.id LIMIT $6::bigint
), school_research_conflicts AS MATERIALIZED (
    SELECT s.id FROM schools s
    WHERE $2::integer=4 AND s.id>$3::bigint
      AND jsonb_typeof(s.address_provenance->'manual_research_conflicts')='array'
      AND jsonb_array_length(s.address_provenance->'manual_research_conflicts')>0
      AND (
        cardinality($1::text[])=0
        OR EXISTS (
            SELECT 1 FROM selected_states selected
            WHERE selected.state=lower(btrim(s.state,
                U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000'))
        )
        OR EXISTS (SELECT 1 FROM scoped_pages p WHERE p.source=s.source AND p.cursor->'context'->>'school_id'=s.source_id)
      )
    ORDER BY s.id LIMIT $6::bigint
), issue_tokens AS MATERIALIZED (
    SELECT 0 AS kind,id AS item_id,0::bigint AS ordinal FROM page_status
    UNION ALL
    SELECT 1,id,ordinal FROM note_tokens
    UNION ALL
    SELECT 2,id,0::bigint FROM unassigned_results
    UNION ALL
    SELECT 3,id,0::bigint FROM invalid_reviewed_results
    UNION ALL
    SELECT 4,id,0::bigint FROM school_research_conflicts
), contents AS (
    SELECT t.kind,t.item_id,t.ordinal,
           CASE WHEN t.kind>=2 AND t.kind<4 THEN r.source WHEN t.kind=4 THEN school.source ELSE p.source END AS source,
           CASE WHEN t.kind>=2 AND t.kind<4 THEN r.source_url WHEN t.kind=4 THEN school.url ELSE p.url END AS url,
           CASE WHEN t.kind>=2 AND t.kind<4 THEN r.status ELSE p.status END AS status,
           p.last_error,
           p.note,
           r.event,r.raw_mark,r.invalidation_reason,school.detail AS school_detail
    FROM issue_tokens t
    LEFT JOIN LATERAL (
        SELECT source,url,status,last_error,
               CASE WHEN t.kind=1 THEN notes[array_lower(notes,1)+(t.ordinal-1)::integer] END AS note
        FROM scrape_pages WHERE t.kind<2 AND id=t.item_id LIMIT 1
    ) p ON true
    LEFT JOIN LATERAL (
        SELECT r.source,r.source_url,r.status,r.event,r.raw_mark,l.invalidation_reason
        FROM results r
        LEFT JOIN invalid_reviewed_result_links l ON l.result_id=r.id AND t.kind=3
        WHERE t.kind>=2 AND t.kind<4 AND r.id=t.item_id LIMIT 1
    ) r ON true
    LEFT JOIN LATERAL (
        SELECT s.source,COALESCE(first_conflict.url,s.source_url) AS url,
               jsonb_build_object(
                   'needs_review',true,
                   'school_source_id',s.source_id,
                   'conflicts',diagnostic.conflicts,
                   'truncated',jsonb_array_length(s.address_provenance->'manual_research_conflicts')>20
               )::text AS detail
        FROM schools s
        LEFT JOIN LATERAL (
            SELECT jsonb_agg(jsonb_build_object(
                'field',left(c.value->>'field',128),
                'reviewed_value',left((c.value->'reviewed_value')::text,512),
                'incoming_value',left((c.value->'incoming_value')::text,512),
                'incoming_source_url',left(c.value->>'incoming_source_url',1024)
            ) ORDER BY c.ordinality) AS conflicts
            FROM jsonb_array_elements(s.address_provenance->'manual_research_conflicts') WITH ORDINALITY c(value,ordinality)
            WHERE c.ordinality<=20
        ) diagnostic ON true
        LEFT JOIN LATERAL (
            SELECT left(c.value->>'incoming_source_url',2048) AS url
            FROM jsonb_array_elements(s.address_provenance->'manual_research_conflicts') WITH ORDINALITY c(value,ordinality)
            WHERE c.ordinality=1 LIMIT 1
        ) first_conflict ON true
        WHERE t.kind=4 AND s.id=t.item_id
    ) school ON true
), measured AS MATERIALIZED (
    SELECT kind,item_id,ordinal,octet_length(source)::bigint+octet_length(url)::bigint+CASE kind
        WHEN 0 THEN 5::bigint+octet_length(status)::bigint
            +CASE WHEN last_error IS NULL THEN 0::bigint ELSE 2::bigint+octet_length(last_error)::bigint END
        WHEN 1 THEN 11::bigint+octet_length(note)::bigint
        ELSE 17::bigint
            +CASE WHEN kind=3 THEN 64::bigint+octet_length(invalidation_reason)::bigint ELSE 0::bigint END
            +CASE WHEN event IS NULL THEN 0::bigint ELSE 3::bigint+octet_length(event)::bigint END
            +CASE WHEN raw_mark IS NULL THEN 0::bigint ELSE 3::bigint+octet_length(raw_mark)::bigint END
            +CASE WHEN status IS NULL THEN 0::bigint ELSE 3::bigint+octet_length(status)::bigint END
            +CASE WHEN kind=4 THEN 64::bigint+octet_length(school_detail)::bigint ELSE 0::bigint END
        END AS bytes
    FROM contents
), budgets AS (
    SELECT *,sum(bytes) OVER (ORDER BY kind,item_id,ordinal)::bigint AS cumulative,
           row_number() OVER (ORDER BY kind,item_id,ordinal) AS position,
           count(*) OVER () AS candidates,
           sum(bytes) OVER ()::bigint AS total_bytes
    FROM measured
), picked AS MATERIALIZED (
    SELECT kind,item_id,ordinal,bytes,
           candidates<$6::bigint AND total_bytes<=$5::bigint AS phase_exhausted
    FROM budgets
    WHERE cumulative<=$5::bigint OR position=1
)
SELECT CASE WHEN t.bytes<=$5::bigint THEN CASE WHEN t.kind>=2 AND t.kind<4 THEN r.source WHEN t.kind=4 THEN school.source ELSE p.source END END AS source,
       CASE WHEN t.bytes<=$5::bigint THEN CASE WHEN t.kind>=2 AND t.kind<4 THEN r.source_url WHEN t.kind=4 THEN school.url ELSE p.url END END AS url,
       CASE WHEN t.bytes<=$5::bigint THEN CASE t.kind
           WHEN 0 THEN concat_ws(': ','page ' || p.status,p.last_error)
           WHEN 1 THEN 'page note: ' || p.note
           WHEN 2 THEN concat_ws(' | ','unassigned result',r.event,r.raw_mark,r.status)
           WHEN 3 THEN concat_ws(' | ','reviewed result link invalidated: '||r.invalidation_reason,r.event,r.raw_mark,r.status)
           ELSE 'manual research address conflict: '||school.detail
       END END AS detail,
       t.kind,t.item_id,t.ordinal,t.bytes,t.phase_exhausted
FROM picked t
LEFT JOIN LATERAL (
    SELECT source,url,status,last_error,
           CASE WHEN t.kind=1 THEN notes[array_lower(notes,1)+(t.ordinal-1)::integer] END AS note
    FROM scrape_pages WHERE t.kind<2 AND t.bytes<=$5::bigint AND id=t.item_id LIMIT 1
) p ON true
LEFT JOIN LATERAL (
    SELECT r.source,r.source_url,r.status,r.event,r.raw_mark,l.invalidation_reason
    FROM results r
    LEFT JOIN invalid_reviewed_result_links l ON l.result_id=r.id AND t.kind=3
    WHERE t.kind>=2 AND t.kind<4 AND t.bytes<=$5::bigint AND r.id=t.item_id LIMIT 1
) r ON true
LEFT JOIN LATERAL (
    SELECT s.source,COALESCE(first_conflict.url,s.source_url) AS url,
           jsonb_build_object(
               'needs_review',true,
               'school_source_id',s.source_id,
               'conflicts',diagnostic.conflicts,
               'truncated',jsonb_array_length(s.address_provenance->'manual_research_conflicts')>20
           )::text AS detail
    FROM schools s
    LEFT JOIN LATERAL (
        SELECT jsonb_agg(jsonb_build_object(
            'field',left(c.value->>'field',128),
            'reviewed_value',left((c.value->'reviewed_value')::text,512),
            'incoming_value',left((c.value->'incoming_value')::text,512),
            'incoming_source_url',left(c.value->>'incoming_source_url',1024)
        ) ORDER BY c.ordinality) AS conflicts
        FROM jsonb_array_elements(s.address_provenance->'manual_research_conflicts') WITH ORDINALITY c(value,ordinality)
        WHERE c.ordinality<=20
    ) diagnostic ON true
    LEFT JOIN LATERAL (
        SELECT left(c.value->>'incoming_source_url',2048) AS url
        FROM jsonb_array_elements(s.address_provenance->'manual_research_conflicts') WITH ORDINALITY c(value,ordinality)
        WHERE c.ordinality=1 LIMIT 1
    ) first_conflict ON true
    WHERE t.kind=4 AND t.bytes<=$5::bigint AND s.id=t.item_id
) school ON true
ORDER BY t.kind,t.item_id,t.ordinal
