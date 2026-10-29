ALTER TABLE scrape_pages DROP CONSTRAINT scrape_pages_attempts_check;
ALTER TABLE scrape_pages ADD CONSTRAINT scrape_pages_attempts_check CHECK (attempts >= 0);

CREATE TABLE durable_page_operations (
    operation_id TEXT PRIMARY KEY CHECK (operation_id ~ '^[0-9a-f]{64}$'),
    page_id BIGINT NOT NULL REFERENCES scrape_pages(id),
    run_generation TEXT NOT NULL,
    binding JSONB NOT NULL,
    state TEXT NOT NULL DEFAULT 'ready' CHECK (state IN ('ready','active','deferred','completed','blocked','rejected','exhausted')),
    cycle INTEGER NOT NULL DEFAULT 1 CHECK (cycle > 0),
    cycle_attempts INTEGER NOT NULL DEFAULT 0 CHECK (cycle_attempts BETWEEN 0 AND 3),
    original_attempts INTEGER NOT NULL CHECK (original_attempts >= 0),
    retry_at_ms BIGINT,
    outcome JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    UNIQUE(page_id,run_generation)
);
CREATE INDEX durable_page_due_idx ON durable_page_operations(run_generation,retry_at_ms,page_id)
    WHERE state IN ('ready','active','deferred');

CREATE TABLE durable_page_attempts (
    attempt_id TEXT PRIMARY KEY CHECK (attempt_id ~ '^[0-9a-f]{64}$'),
    operation_id TEXT NOT NULL REFERENCES durable_page_operations(operation_id),
    cycle INTEGER NOT NULL CHECK (cycle > 0),
    ordinal INTEGER NOT NULL CHECK (ordinal BETWEEN 1 AND 3),
    total_attempts INTEGER NOT NULL CHECK (total_attempts > 0),
    state TEXT NOT NULL DEFAULT 'reserved' CHECK (state IN ('reserved','captured','parsed','completed','failed')),
    capture JSONB,
    parsed JSONB,
    outcome JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    UNIQUE(operation_id,cycle,ordinal)
);

CREATE TABLE durable_page_receipts (
    operation_id TEXT PRIMARY KEY REFERENCES durable_page_operations(operation_id),
    attempt_id TEXT NOT NULL UNIQUE REFERENCES durable_page_attempts(attempt_id),
    page_id BIGINT NOT NULL REFERENCES scrape_pages(id),
    capture JSONB NOT NULL,
    parsed JSONB NOT NULL,
    receipt_sha256 TEXT NOT NULL CHECK (receipt_sha256 ~ '^[0-9a-f]{64}$'),
    outcome JSONB NOT NULL,
    committed_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
