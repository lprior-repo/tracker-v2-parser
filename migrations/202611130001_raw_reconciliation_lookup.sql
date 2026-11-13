CREATE INDEX results_milesplit_meet_hash_idx
    ON results USING HASH(meet_id)
    WHERE source='milesplit';

CREATE INDEX results_milesplit_observed_file_hash_idx
    ON results USING HASH((raw#>>'{source_values,published_file_identity}'))
    WHERE source='milesplit' AND raw#>>'{source_values,kind}'='raw_result';

CREATE INDEX results_milesplit_unassigned_file_hash_idx
    ON results USING HASH((raw#>>'{raw,published_file_identity}'))
    WHERE source='milesplit' AND raw#>>'{raw,kind}'='raw_result';

ANALYZE results;
