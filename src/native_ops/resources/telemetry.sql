SELECT jsonb_build_object(
 'observed_at',clock_timestamp(),
 'postmaster_started_at',pg_postmaster_start_time(),
 'postgres_major',current_setting('server_version_num')::integer/10000,
 'database', (SELECT to_jsonb(t) FROM (SELECT datname,pg_database_size(datname) database_bytes,numbackends,xact_commit,xact_rollback,blks_read,blks_hit,temp_files,temp_bytes,deadlocks,blk_read_time,blk_write_time,stats_reset FROM pg_stat_database WHERE datname=current_database())t),
 'settings',(SELECT jsonb_object_agg(name,jsonb_build_object('value',setting,'unit',unit,'context',context,'source',source,'pending_restart',pending_restart)) FROM pg_settings WHERE name=ANY(ARRAY['shared_buffers','work_mem','maintenance_work_mem','autovacuum_work_mem','max_connections','effective_cache_size','max_parallel_workers_per_gather','io_method','io_workers','effective_io_concurrency','max_wal_size','checkpoint_timeout','checkpoint_completion_target','track_io_timing','track_wal_io_timing','fsync','full_page_writes','synchronous_commit','shared_preload_libraries'])),
 'checkpointer',(SELECT to_jsonb(t) FROM pg_stat_checkpointer t),
 'wal',(SELECT to_jsonb(t) FROM pg_stat_wal t),
 'io',(SELECT jsonb_agg(to_jsonb(t)) FROM (SELECT backend_type,object,context,reads,read_bytes,read_time,writes,write_bytes,write_time,evictions,fsyncs,fsync_time,stats_reset FROM pg_stat_io ORDER BY backend_type,object,context LIMIT 64)t),
 'tables',(SELECT jsonb_agg(to_jsonb(t)) FROM (SELECT schemaname,relname,n_live_tup,n_dead_tup,n_mod_since_analyze,n_ins_since_vacuum,last_autovacuum,last_autoanalyze,autovacuum_count,autoanalyze_count FROM pg_stat_user_tables WHERE schemaname=ANY(current_schemas(false)) ORDER BY n_live_tup DESC,relname LIMIT 20)t),
 'activity',(SELECT jsonb_agg(to_jsonb(t)) FROM (SELECT pid,backend_type,state,wait_event_type,wait_event,EXTRACT(epoch FROM clock_timestamp()-xact_start) transaction_seconds,pg_blocking_pids(pid) blocking_pids FROM pg_stat_activity WHERE datname=current_database() ORDER BY pid LIMIT 40)t),
 'pg_stat_statements_installed',EXISTS(SELECT 1 FROM pg_extension WHERE extname='pg_stat_statements'),
 'transaction_read_only',current_setting('transaction_read_only')
)
