ALTER TABLE contacts ALTER COLUMN email DROP NOT NULL;
ALTER TABLE contacts ADD COLUMN source_key TEXT;

UPDATE contacts
SET source_key = encode(sha256(convert_to(
    jsonb_build_array(source_url, name, role, sport, lower(btrim(email)))::text,
    'UTF8'
)), 'hex');

ALTER TABLE contacts
    ALTER COLUMN source_key SET NOT NULL,
    ADD CONSTRAINT contacts_source_key_check CHECK (source_key ~ '^[0-9a-f]{64}$'),
    ADD CONSTRAINT contacts_published_identity_check CHECK (
        COALESCE(name ~ '[^[:space:]]', FALSE) OR email IS NOT NULL
    ),
    DROP CONSTRAINT contacts_school_id_kind_email_key_role_key,
    ADD CONSTRAINT contacts_school_kind_source_key UNIQUE (school_id, kind, source_key);
