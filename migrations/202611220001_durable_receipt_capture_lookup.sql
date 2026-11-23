CREATE INDEX durable_page_receipts_capture_url_hash_idx
    ON durable_page_receipts USING HASH((capture->>'url'));

ANALYZE durable_page_receipts;
