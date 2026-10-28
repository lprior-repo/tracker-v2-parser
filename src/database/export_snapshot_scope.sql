SELECT a.id
FROM athletes a
LEFT JOIN LATERAL (
    SELECT bool_or(NULLIF(state,'') IS NOT NULL) AS has_state,
           bool_or(EXISTS (
               SELECT 1 FROM unnest($1::text[]) selected(state)
               WHERE lower(btrim(selected.state, U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000'))=school_states.state
           )) AS matches_state
    FROM (
        SELECT lower(btrim(s.state, U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000')) AS state
        FROM schools s
        WHERE s.id IN (
            SELECT p.school_id FROM athlete_sources p WHERE p.athlete_id=a.id AND p.school_id IS NOT NULL
        )
    ) school_states
) ss ON true
WHERE a.id > $3::bigint AND a.name IS NOT NULL
  AND (cardinality($1::text[])=0 OR CASE WHEN COALESCE(ss.has_state,false)
       THEN COALESCE(ss.matches_state,false)
       ELSE NULLIF(btrim(a.state,U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000'),'') IS NULL
            OR EXISTS (
                SELECT 1 FROM unnest($1::text[]) selected(state)
                WHERE lower(btrim(selected.state,U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000'))=
                      lower(btrim(a.state,U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000'))
            ) END)
  AND (cardinality($2::integer[])=0 OR a.graduation_year=ANY($2) OR a.graduation_year IS NULL)
ORDER BY a.id LIMIT 1
