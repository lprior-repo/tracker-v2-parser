CREATE INDEX IF NOT EXISTS scrape_pages_blocked_source_idx ON scrape_pages(source,id)
    WHERE status='blocked' AND cursor->>'kind'<>'contacts';

CREATE INDEX IF NOT EXISTS durable_page_receipts_page_idx ON durable_page_receipts(page_id);
