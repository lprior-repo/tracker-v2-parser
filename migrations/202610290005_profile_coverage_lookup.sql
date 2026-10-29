CREATE INDEX scrape_pages_profile_coverage_lookup_idx
    ON scrape_pages(source, (cursor->'context'->>'athlete_id'), id)
    WHERE cursor->>'kind'='athlete'
      AND url ~ '^https://(www|[a-z]{2})[.]milesplit[.]com/athletes/[1-9][0-9]*(-[^/?#%]+)?(/(stats|progression))?/?$';
