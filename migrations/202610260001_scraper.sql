CREATE TABLE schools (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    source TEXT NOT NULL CHECK (source IN ('athleticnet', 'milesplit')),
    source_id TEXT NOT NULL CHECK (btrim(source_id) <> ''),
    name TEXT NOT NULL CHECK (btrim(name) <> ''),
    state TEXT, city TEXT, address TEXT, postal_code TEXT, website TEXT,
    public_email TEXT, source_url TEXT NOT NULL,
    last_checked TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (source, source_id)
);

CREATE TABLE athletes (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name TEXT CHECK (name IS NULL OR btrim(name) <> ''),
    gender TEXT, grade TEXT, graduation_year INTEGER CHECK (graduation_year BETWEEN 1900 AND 2200),
    school_id BIGINT REFERENCES schools(id), state TEXT, category TEXT,
    needs_review BOOLEAN NOT NULL DEFAULT FALSE
);

CREATE TABLE athlete_sources (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    athlete_id BIGINT NOT NULL REFERENCES athletes(id),
    source TEXT NOT NULL CHECK (source IN ('athleticnet', 'milesplit')),
    source_id TEXT NOT NULL CHECK (btrim(source_id) <> ''),
    profile_url TEXT,
    name TEXT CHECK (name IS NULL OR btrim(name) <> ''),
    school_id BIGINT REFERENCES schools(id), state TEXT, gender TEXT, grade TEXT,
    graduation_year INTEGER CHECK (graduation_year BETWEEN 1900 AND 2200),
    details JSONB NOT NULL DEFAULT '{}'::jsonb,
    last_seen TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (source, source_id),
    UNIQUE (athlete_id, source)
);
CREATE INDEX athlete_sources_school_idx ON athlete_sources(school_id);

CREATE TABLE results (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    source_result_key TEXT NOT NULL UNIQUE CHECK (length(source_result_key) = 64),
    athlete_id BIGINT REFERENCES athletes(id),
    athlete_source_id BIGINT REFERENCES athlete_sources(id),
    represented_school_id BIGINT REFERENCES schools(id),
    source TEXT NOT NULL CHECK (source IN ('athleticnet', 'milesplit')),
    source_result_id TEXT,
    event TEXT NOT NULL CHECK (btrim(event) <> ''),
    raw_mark TEXT NOT NULL CHECK (btrim(raw_mark) <> ''),
    normalized_numeric BIGINT CHECK (normalized_numeric > 0),
    normalized_unit TEXT CHECK (normalized_unit IN ('ns', 'um', 'centipoints')),
    sport TEXT NOT NULL CHECK (sport IN ('indoor', 'outdoor', 'xc')),
    meet_id TEXT, meet_name TEXT, meet_date TEXT,
    wind DOUBLE PRECISION CHECK (wind IS NULL OR (wind > '-Infinity'::float8 AND wind < 'Infinity'::float8)),
    timing TEXT, specification TEXT, course TEXT,
    status TEXT NOT NULL,
    is_personal_record BOOLEAN NOT NULL, is_relay BOOLEAN NOT NULL,
    season INTEGER, grade TEXT, gender TEXT,
    source_url TEXT NOT NULL,
    last_seen TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    raw JSONB NOT NULL,
    CHECK ((athlete_source_id IS NULL) = (athlete_id IS NULL)),
    CHECK ((normalized_numeric IS NULL) = (normalized_unit IS NULL))
);
CREATE INDEX results_athlete_source_idx ON results(athlete_source_id);
CREATE INDEX results_athlete_idx ON results(athlete_id);
CREATE INDEX results_school_idx ON results(represented_school_id);
CREATE INDEX results_event_idx ON results(sport, event);

CREATE TABLE contacts (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    school_id BIGINT NOT NULL REFERENCES schools(id),
    kind TEXT NOT NULL CHECK (kind IN ('coach', 'school_office')),
    name TEXT, role TEXT NOT NULL,
    email TEXT NOT NULL CHECK (email ~ '^[^[:space:]@]+@[^[:space:]@]+\.[^[:space:]@]+$'),
    email_key TEXT GENERATED ALWAYS AS (lower(btrim(email))) STORED,
    sport TEXT CHECK (sport IN ('indoor', 'outdoor', 'xc')),
    source_url TEXT NOT NULL,
    effective_from TEXT, effective_until TEXT,
    last_checked TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    raw JSONB NOT NULL,
    UNIQUE (school_id, kind, email_key, role)
);
CREATE INDEX contacts_school_idx ON contacts(school_id);

CREATE TABLE scrape_pages (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    source TEXT NOT NULL CHECK (source IN ('athleticnet', 'milesplit')),
    page_key TEXT NOT NULL CHECK (length(page_key) = 64),
    url TEXT NOT NULL,
    cursor JSONB NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'succeeded', 'failed', 'blocked')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts BETWEEN 0 AND 3),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    last_success TIMESTAMPTZ,
    last_error TEXT,
    UNIQUE (source, page_key)
);
CREATE INDEX scrape_pages_pending_idx ON scrape_pages(source, id)
    WHERE status IN ('pending', 'failed') AND attempts < 3;
COMMENT ON TABLE athletes IS 'Canonical identity joins only explicit cross-provider profile links; ambiguities remain separate and flagged for review.';
COMMENT ON TABLE scrape_pages IS 'One-process crawler checkpoints; request context, discovered pages and parsed records commit atomically with success. Attempts persist across resume.';
