CREATE INDEX results_milesplit_source_result_id_hash_idx
    ON results USING HASH(source_result_id)
    WHERE source='milesplit';

CREATE INDEX results_milesplit_source_url_hash_idx
    ON results USING HASH(source_url)
    WHERE source='milesplit';

ANALYZE results;
