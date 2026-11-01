INSERT INTO schools(source,source_id,name,state,source_url)
SELECT 'milesplit',(1000+n)::text,'Scope School '||n,'MI','https://mi.milesplit.com/teams/'||(1000+n)
FROM generate_series(1,12) n;
INSERT INTO athletes(name,graduation_year,state,school_id)
SELECT 'Scope Runner '||n,CASE n%4 WHEN 0 THEN NULL WHEN 1 THEN 2027 WHEN 2 THEN 2028 ELSE 2026 END,
       'MI',(SELECT id FROM schools WHERE source_id=(1000+((n-1)%12)+1)::text)
FROM generate_series(1,48) n;
INSERT INTO athlete_sources(athlete_id,source,source_id,profile_url,school_id,graduation_year)
SELECT id,'milesplit',(10000+id)::text,'https://mi.milesplit.com/athletes/'||(10000+id)||'-runner',school_id,
       CASE id%5 WHEN 0 THEN 2027 ELSE graduation_year END FROM athletes;
INSERT INTO scrape_pages(source,page_key,url,cursor,status,attempts)
SELECT 'milesplit',lpad(to_hex(row_number() OVER ()+0),64,'0'),url,jsonb_build_object('source','milesplit','kind','athlete','url',url,
       'context',jsonb_build_object('athlete_id',p.source_id,'school_id',s.source_id,'state',v.state)
           ||v.acquisition||CASE variant WHEN 1 THEN '{"sport":"xc"}'::jsonb ELSE '{}'::jsonb END),
       CASE WHEN section=0 AND p.athlete_id%3=0 THEN 'succeeded'
            WHEN p.athlete_id%7=0 THEN 'blocked'
            WHEN p.athlete_id%5=0 THEN 'failed' ELSE 'pending' END,(p.athlete_id%4)::int
FROM athlete_sources p JOIN schools s ON s.id=p.school_id
CROSS JOIN (VALUES (0,''),(1,'/stats'),(2,'/progression')) f(section,suffix)
CROSS JOIN LATERAL (SELECT p.profile_url||suffix AS url) u
CROSS JOIN (VALUES
 (1,'MI','{}'::jsonb),(2,'WI','{"acquisition_state":" mi "}'::jsonb),
 (3,'MI','{"acquisition_state":""}'::jsonb),(4,'MI','{"acquisition_state":null}'::jsonb),
 (5,'MI','{"acquisition_state":"WI"}'::jsonb),(6,NULL,'{}'::jsonb),
 (7,'mi','{"acquisition_state":27}'::jsonb),(8,'MI','{"acquisition_state":{}}'::jsonb),
 (9,' mi ','{}'::jsonb),(10,'WI','{}'::jsonb)) v(variant,state,acquisition);
INSERT INTO scrape_pages(source,page_key,url,cursor,status,attempts)
SELECT 'milesplit',lpad(to_hex(row_number() OVER ()+100000),64,'0'),url,jsonb_build_object('source','milesplit','kind',kind,'url',url,
       'context',jsonb_build_object('school_id',s.source_id,'state',v.state)||v.acquisition),
       CASE WHEN s.id%4=0 AND kind='team' THEN 'succeeded' ELSE 'pending' END,(s.id%4)::int
FROM schools s CROSS JOIN (VALUES ('team',''),('roster','/roster'),('contacts','/staff')) k(kind,suffix)
CROSS JOIN (VALUES ('MI','{}'::jsonb),('WI','{"acquisition_state":"MI"}'::jsonb),
 ('MI','{"acquisition_state":""}'::jsonb),(NULL,'{}'::jsonb)) v(state,acquisition)
CROSS JOIN LATERAL (SELECT CASE WHEN kind='contacts' THEN 'https://school'||s.source_id||'.example.edu/staff'
       ELSE 'https://mi.milesplit.com/teams/'||s.source_id||suffix END AS url) u;
INSERT INTO scrape_pages(source,page_key,url,cursor,status,attempts)
SELECT 'milesplit',lpad(to_hex(row_number() OVER ()+200000),64,'0'),url,jsonb_build_object('source',CASE defect WHEN 1 THEN 'athleticnet' ELSE 'milesplit' END,
       'kind','athlete','url',CASE defect WHEN 2 THEN url||'/mismatch' ELSE url END,
       'context',jsonb_build_object('athlete_id',CASE defect WHEN 3 THEN 'bad' ELSE p.source_id END,'state','MI'))
       ||CASE defect WHEN 4 THEN '{"method":"POST"}'::jsonb WHEN 5 THEN '{"body":{}}'::jsonb
          WHEN 6 THEN '{"token_url":"https://token.example"}'::jsonb
          WHEN 7 THEN '{"browser_origin":"https://origin.example"}'::jsonb ELSE '{}'::jsonb END,
       'pending',0
FROM athlete_sources p CROSS JOIN generate_series(1,8) defect
CROSS JOIN LATERAL (SELECT p.profile_url||CASE defect WHEN 8 THEN '/photos' ELSE '' END AS url) u;
INSERT INTO durable_page_operations(operation_id,page_id,run_generation,binding,state,original_attempts,cycle,cycle_attempts,retry_at_ms)
SELECT lpad(to_hex(id),64,'0'),id,'parity-generation','{}',
       CASE id%7 WHEN 0 THEN 'active' WHEN 1 THEN 'completed' WHEN 2 THEN 'deferred'
          WHEN 3 THEN 'rejected' WHEN 4 THEN 'blocked' WHEN 5 THEN 'exhausted' ELSE 'ready' END,
       attempts,1,(id%4)::int,CASE WHEN id%2=0 THEN 2000 ELSE 500 END
FROM scrape_pages WHERE id%3=0;
INSERT INTO durable_page_operations(operation_id,page_id,run_generation,binding,state,original_attempts,cycle,cycle_attempts)
SELECT lpad(to_hex(p.id),64,'0'),p.id,'parity-generation','{}','active',p.attempts,1,0
FROM scrape_pages p
WHERE p.id IN (SELECT min(id) FROM scrape_pages WHERE cursor->>'kind' IN ('team','roster','contacts') GROUP BY cursor->>'kind')
ON CONFLICT(page_id,run_generation) DO UPDATE SET state='active';
