WITH canonical_record AS MATERIALIZED (
    SELECT document,octet_length(document::text)::bigint AS original_bytes
    FROM (__RECORDS__) report(document)
), projected_record AS (
    SELECT original_bytes,jsonb_build_object(
      'id',document->'id','name',document->'name','gender',document->'gender','grade',document->'grade',
      'graduation_year',document->'graduation_year','needs_review',document->'needs_review',
      'schools',document->'schools','coaches',document->'coaches','profiles',document->'profiles',
      'profile_details',
      COALESCE((SELECT jsonb_agg(jsonb_build_object('source',profile.value->'source','source_id',profile.value->'source_id',
        'url',profile.value->'url','source_values',
        jsonb_build_object('pages',COALESCE((
          SELECT jsonb_object_agg(page.key,jsonb_build_object('pr_rows',rows.value))
          FROM jsonb_each(CASE WHEN profile.value->>'source'='milesplit'
            AND jsonb_typeof(profile.value#>'{source_values,pages}')='object'
            THEN profile.value#>'{source_values,pages}' ELSE '{}'::jsonb END) page
          CROSS JOIN LATERAL (
            SELECT jsonb_agg(jsonb_build_object('html',row.value->'html',
              'masked',row.value->'masked') ORDER BY row.ordinality) AS value
            FROM jsonb_array_elements(CASE WHEN jsonb_typeof(page.value->'pr_rows')='array'
              THEN page.value->'pr_rows' ELSE '[]'::jsonb END) WITH ORDINALITY row(value,ordinality)
            WHERE jsonb_typeof(row.value->'html')='string'
          ) rows WHERE rows.value IS NOT NULL
        ),'{}'::jsonb))) ORDER BY profile.ordinality)
        FROM jsonb_array_elements(document->'profile_details') WITH ORDINALITY profile(value,ordinality)),
        '[]'::jsonb),
      'performances',COALESCE((SELECT jsonb_agg(jsonb_set(evidence.value,
        '{performance,source_values}',COALESCE((
          SELECT jsonb_object_agg(field.key,field.value)
          FROM jsonb_each(CASE WHEN jsonb_typeof(evidence.value#>'{performance,source_values}')='object'
            THEN evidence.value#>'{performance,source_values}' ELSE '{}'::jsonb END) field
          WHERE field.key IN ('level','revision_status')
        ),'null'::jsonb)) ORDER BY evidence.ordinality)
        FROM jsonb_array_elements(document->'performances') WITH ORDINALITY evidence(value,ordinality)),
        '[]'::jsonb)) AS document
    FROM canonical_record
)
SELECT CASE WHEN original_bytes <= $4::bigint THEN document::text END,original_bytes
FROM projected_record
