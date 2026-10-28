CREATE INDEX scrape_pages_unfinished_contact_authority_idx ON scrape_pages USING HASH (
    regexp_replace(
        lower(substring(url from '(?i)^https?://([^/?#]+)')),
        CASE WHEN lower(url) LIKE 'https://%' THEN ':443$' ELSE ':80$' END,
        ''
    )
) WHERE status IN ('pending','failed') AND cursor->>'kind'='contacts';
