CREATE TABLE durable_wire_requests (
    wire_id TEXT PRIMARY KEY CHECK (wire_id ~ '^[0-9a-f]{64}$'),
    operation_id TEXT NOT NULL CHECK (operation_id ~ '^[0-9a-f]{64}$'),
    attempt_id TEXT NOT NULL CHECK (attempt_id ~ '^[0-9a-f]{64}$'),
    request_sha256 TEXT NOT NULL CHECK (request_sha256 ~ '^[0-9a-f]{64}$'),
    occurrence INTEGER NOT NULL CHECK (occurrence BETWEEN 1 AND 512),
    source TEXT NOT NULL CHECK (source IN ('athleticnet', 'milesplit')),
    origin TEXT NOT NULL CHECK (length(origin) BETWEEN 1 AND 2048),
    method TEXT NOT NULL CHECK (method IN ('GET', 'POST', 'HEAD', 'OPTIONS')),
    state TEXT NOT NULL CHECK (state IN ('intent', 'headers', 'failed')),
    status INTEGER CHECK (status BETWEEN 100 AND 599),
    error_class TEXT CHECK (error_class = 'transport'),
    started_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    finished_at TIMESTAMPTZ,
    UNIQUE (operation_id, attempt_id, request_sha256, occurrence),
    CHECK (
        (state = 'intent' AND status IS NULL AND error_class IS NULL AND finished_at IS NULL)
        OR (state = 'headers' AND status IS NOT NULL AND error_class IS NULL AND finished_at IS NOT NULL)
        OR (state = 'failed' AND status IS NULL AND error_class IS NOT NULL AND finished_at IS NOT NULL)
    )
);

CREATE INDEX durable_wire_requests_attempt_idx ON durable_wire_requests (operation_id, attempt_id);
CREATE INDEX durable_wire_requests_stopped_origin_idx ON durable_wire_requests (origin)
    WHERE status IN (401, 402, 403, 429, 451);
