CREATE INDEX durable_wire_requests_pending_source_started_idx
ON durable_wire_requests(source,started_at)
WHERE state='intent';

ALTER TABLE durable_wire_requests
ADD COLUMN execute_started_at TIMESTAMPTZ;
