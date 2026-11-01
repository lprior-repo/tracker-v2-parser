CREATE INDEX scrape_pages_profile_scope_lookup_idx
    ON scrape_pages(source,
        (left(upper(btrim(COALESCE(cursor->'context'->>'acquisition_state',cursor->'context'->>'state'))),2)),
        (cursor->>'kind'),id)
    WHERE source='milesplit' AND cursor->>'kind' IN ('athlete','team','roster','contacts');

ANALYZE scrape_pages;
