CREATE INDEX scrape_pages_contacts_url_idx ON scrape_pages USING HASH(url)
    WHERE cursor->>'kind' = 'contacts';

CREATE INDEX scrape_pages_blocked_contacts_idx ON scrape_pages(id)
    WHERE status = 'blocked' AND cursor->>'kind' = 'contacts';
