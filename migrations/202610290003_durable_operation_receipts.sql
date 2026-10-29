CREATE TABLE durable_operation_receipts (
    operation_key TEXT PRIMARY KEY CHECK (octet_length(operation_key) BETWEEN 1 AND 512),
    input_sha256 TEXT NOT NULL CHECK (input_sha256 ~ '^[0-9a-f]{64}$'),
    result JSONB NOT NULL CHECK (octet_length(result::text) <= 1048576),
    committed_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
