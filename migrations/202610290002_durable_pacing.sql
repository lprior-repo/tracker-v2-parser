CREATE TABLE durable_wire_pacing (
    pacing_key TEXT PRIMARY KEY CHECK (length(pacing_key) BETWEEN 1 AND 2048),
    next_allowed_at TIMESTAMPTZ NOT NULL DEFAULT '1970-01-01 00:00:00+00',
    lease_token TEXT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CHECK (lease_token IS NULL OR length(lease_token) = 32)
);
