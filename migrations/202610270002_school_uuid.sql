ALTER TABLE schools ADD COLUMN uuid UUID NOT NULL DEFAULT gen_random_uuid();
ALTER TABLE schools ADD CONSTRAINT schools_uuid_key UNIQUE (uuid);
