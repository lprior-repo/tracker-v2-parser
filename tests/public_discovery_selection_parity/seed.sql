INSERT INTO scrape_pages(source,page_key,url,cursor,status,attempts)
SELECT 'milesplit',lpad(to_hex(500000+n),64,'0'),url,
       jsonb_build_object('source','milesplit','kind','athlete','url',url,
         'context',jsonb_build_object('athlete_id',(900000+n/3)::text,'school_id',(1001+n%12)::text,
           'state',CASE n%3 WHEN 0 THEN 'IA' WHEN 1 THEN 'MI' ELSE 'WI' END))
         ||CASE n%11 WHEN 0 THEN '{"method":"POST"}'::jsonb WHEN 1 THEN '{"body":{}}'::jsonb
           WHEN 2 THEN '{"token_url":"https://token.example"}'::jsonb ELSE '{}'::jsonb END,
       CASE n%13 WHEN 0 THEN 'blocked' WHEN 1 THEN 'failed' ELSE 'pending' END,n%4
FROM generate_series(1,120) n
CROSS JOIN LATERAL (SELECT 'https://ia.milesplit.com/athletes/'||(900000+n/3)||CASE n%7 WHEN 0 THEN '/stats' ELSE '-runner' END AS url) u;
INSERT INTO durable_page_operations(operation_id,page_id,run_generation,binding,state,original_attempts,cycle,cycle_attempts,retry_at_ms)
SELECT lpad(to_hex(id),64,'0'),id,'parity-generation','{}',
       CASE id%7 WHEN 0 THEN 'active' WHEN 1 THEN 'completed' WHEN 2 THEN 'deferred'
         WHEN 3 THEN 'rejected' WHEN 4 THEN 'blocked' WHEN 5 THEN 'exhausted' ELSE 'ready' END,
       attempts,1,id%4,CASE WHEN id%2=0 THEN 2000 ELSE 500 END
FROM scrape_pages WHERE page_key>=lpad(to_hex(500000),64,'0') AND id%2=0;
