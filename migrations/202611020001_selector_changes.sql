CREATE TABLE selector_changes (
    kind TEXT NOT NULL,
    identity TEXT NOT NULL,
    auxiliary BOOLEAN NOT NULL,
    PRIMARY KEY(kind,identity),
    changed_xid xid8 NOT NULL
);
CREATE INDEX selector_changes_xid_idx ON selector_changes(changed_xid,kind,identity);

CREATE FUNCTION selector_changed(entity TEXT,entity_id TEXT,aux BOOLEAN) RETURNS VOID LANGUAGE SQL AS $$
    INSERT INTO selector_changes(kind,identity,changed_xid,auxiliary)
    VALUES(entity,entity_id,pg_current_xact_id(),aux)
    ON CONFLICT(kind,identity) DO UPDATE SET changed_xid=EXCLUDED.changed_xid,
        auxiliary=selector_changes.auxiliary OR EXCLUDED.auxiliary
    WHERE selector_changes.changed_xid IS DISTINCT FROM EXCLUDED.changed_xid
       OR (NOT selector_changes.auxiliary AND EXCLUDED.auxiliary);
$$;

CREATE FUNCTION selector_page_change() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP='UPDATE' AND (OLD.source,OLD.url,OLD.cursor,OLD.status,OLD.attempts)
        IS NOT DISTINCT FROM (NEW.source,NEW.url,NEW.cursor,NEW.status,NEW.attempts) THEN RETURN NULL; END IF;
    IF TG_OP='DELETE' THEN
        PERFORM selector_changed('page',OLD.id::TEXT,COALESCE(OLD.cursor->>'kind'='contacts',false));
    ELSE
        PERFORM selector_changed('page',NEW.id::TEXT,COALESCE(NEW.cursor->>'kind'='contacts',false)
            OR (TG_OP='UPDATE' AND COALESCE(OLD.cursor->>'kind'='contacts',false)));
    END IF;
    RETURN NULL;
END $$;
CREATE TRIGGER selector_page_change AFTER INSERT OR UPDATE OR DELETE ON scrape_pages
    FOR EACH ROW EXECUTE FUNCTION selector_page_change();

CREATE FUNCTION selector_operation_change() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP='UPDATE' AND (OLD.page_id,OLD.run_generation,OLD.state,OLD.cycle,OLD.cycle_attempts,OLD.retry_at_ms)
        IS NOT DISTINCT FROM (NEW.page_id,NEW.run_generation,NEW.state,NEW.cycle,NEW.cycle_attempts,NEW.retry_at_ms) THEN RETURN NULL; END IF;
    PERFORM selector_changed('operation',COALESCE(NEW.operation_id,OLD.operation_id),false);
    RETURN NULL;
END $$;
CREATE TRIGGER selector_operation_change AFTER INSERT OR UPDATE OR DELETE ON durable_page_operations
    FOR EACH ROW EXECUTE FUNCTION selector_operation_change();

CREATE FUNCTION selector_source_change() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP='UPDATE' AND (OLD.athlete_id,OLD.source,OLD.source_id,OLD.profile_url,OLD.school_id,OLD.graduation_year)
        IS NOT DISTINCT FROM (NEW.athlete_id,NEW.source,NEW.source_id,NEW.profile_url,NEW.school_id,NEW.graduation_year) THEN RETURN NULL; END IF;
    PERFORM selector_changed('profile',COALESCE(NEW.id,OLD.id)::TEXT,false);
    RETURN NULL;
END $$;
CREATE TRIGGER selector_source_change AFTER INSERT OR UPDATE OR DELETE ON athlete_sources
    FOR EACH ROW EXECUTE FUNCTION selector_source_change();

CREATE FUNCTION selector_athlete_change() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP='UPDATE' AND OLD.graduation_year IS NOT DISTINCT FROM NEW.graduation_year THEN RETURN NULL; END IF;
    PERFORM selector_changed('athlete',COALESCE(NEW.id,OLD.id)::TEXT,false);
    RETURN NULL;
END $$;
CREATE TRIGGER selector_athlete_change AFTER INSERT OR UPDATE OR DELETE ON athletes
    FOR EACH ROW EXECUTE FUNCTION selector_athlete_change();

CREATE FUNCTION selector_school_change() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP='UPDATE' AND (OLD.source,OLD.source_id,OLD.address,OLD.city,OLD.state,OLD.postal_code)
        IS NOT DISTINCT FROM (NEW.source,NEW.source_id,NEW.address,NEW.city,NEW.state,NEW.postal_code) THEN RETURN NULL; END IF;
    IF TG_OP<>'UPDATE' OR (OLD.source,OLD.source_id) IS DISTINCT FROM (NEW.source,NEW.source_id) THEN
        PERFORM selector_changed('school',COALESCE(NEW.id,OLD.id)::TEXT,true);
    ELSE
        PERFORM selector_changed('school_contact',NEW.id::TEXT,true);
    END IF;
    RETURN NULL;
END $$;
CREATE TRIGGER selector_school_change AFTER INSERT OR UPDATE OR DELETE ON schools
    FOR EACH ROW EXECUTE FUNCTION selector_school_change();

CREATE FUNCTION selector_contact_change() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP='UPDATE' AND (OLD.school_id,OLD.kind,OLD.name,OLD.email,OLD.raw,OLD.role,OLD.sport,OLD.source_url)
        IS NOT DISTINCT FROM (NEW.school_id,NEW.kind,NEW.name,NEW.email,NEW.raw,NEW.role,NEW.sport,NEW.source_url) THEN RETURN NULL; END IF;
    PERFORM selector_changed('contact',COALESCE(NEW.id,OLD.id)::TEXT,true);
    RETURN NULL;
END $$;
CREATE TRIGGER selector_contact_change AFTER INSERT OR UPDATE OR DELETE ON contacts
    FOR EACH ROW EXECUTE FUNCTION selector_contact_change();

CREATE FUNCTION selector_truncated() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    PERFORM selector_changed('reset',TG_TABLE_NAME,true);
    RETURN NULL;
END $$;
CREATE TRIGGER selector_truncated AFTER TRUNCATE ON scrape_pages FOR EACH STATEMENT EXECUTE FUNCTION selector_truncated();
CREATE TRIGGER selector_truncated AFTER TRUNCATE ON durable_page_operations FOR EACH STATEMENT EXECUTE FUNCTION selector_truncated();
CREATE TRIGGER selector_truncated AFTER TRUNCATE ON athlete_sources FOR EACH STATEMENT EXECUTE FUNCTION selector_truncated();
CREATE TRIGGER selector_truncated AFTER TRUNCATE ON athletes FOR EACH STATEMENT EXECUTE FUNCTION selector_truncated();
CREATE TRIGGER selector_truncated AFTER TRUNCATE ON schools FOR EACH STATEMENT EXECUTE FUNCTION selector_truncated();
CREATE TRIGGER selector_truncated AFTER TRUNCATE ON contacts FOR EACH STATEMENT EXECUTE FUNCTION selector_truncated();
