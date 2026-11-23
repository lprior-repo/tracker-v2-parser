INSERT INTO schools(source,source_id,name,state,source_url)
VALUES('milesplit','900001','Synthetic audit school','WI','https://wi.milesplit.com/teams/900001');

INSERT INTO scrape_pages(source,page_key,url,cursor,status,last_error)
SELECT 'milesplit',lpad(to_hex(g),64,'0'),target_url,
       jsonb_build_object('source','milesplit','kind',CASE WHEN g<=50000 THEN 'results' ELSE 'athlete' END,
           'url',target_url,'context',jsonb_build_object('school_id','900001',
               'acquisition_state',CASE g%5 WHEN 0 THEN U&'\2003\00A0\0009' WHEN 1 THEN '' WHEN 2 THEN 'OH' WHEN 3 THEN U&'\2003oH\00A0' ELSE 'MI' END,
               'state',CASE WHEN g%5=1 THEN U&'\0009\2003' ELSE 'WI' END)),
       CASE WHEN g<=50000 THEN 'succeeded' ELSE 'failed' END,
       CASE WHEN g>50000 THEN 'Synthetic scope fallback failure' END
FROM generate_series(1,55000) g
CROSS JOIN LATERAL (
    SELECT CASE WHEN g<=50000 THEN 'https://oh.milesplit.com/meets/'||g||'/results/'||g||'/raw'
        ELSE 'https://wi.milesplit.com/athletes/'||g||'/stats' END AS target_url
) urls ORDER BY g;

INSERT INTO durable_page_operations(operation_id,page_id,run_generation,binding,state,original_attempts)
SELECT lpad(to_hex(id),64,'0'),id,'audit4','{}','completed',0 FROM scrape_pages WHERE id<=50000;
INSERT INTO durable_page_attempts(attempt_id,operation_id,cycle,ordinal,total_attempts,state)
SELECT lpad(to_hex(page_id+100000),64,'0'),operation_id,1,1,1,'completed' FROM durable_page_operations;
INSERT INTO durable_page_receipts(operation_id,attempt_id,page_id,capture,parsed,receipt_sha256,outcome)
SELECT o.operation_id,a.attempt_id,o.page_id,
       jsonb_build_object('url','https://www.milesplit.com/meets/'||o.page_id||'/results/'||o.page_id||'/raw?retained='||repeat('a',160)),
       '{}',repeat('0',64),'{}'
FROM durable_page_operations o JOIN durable_page_attempts a USING(operation_id);
INSERT INTO results(source_result_key,source,event,raw_mark,sport,status,is_personal_record,is_relay,source_url,raw)
SELECT lpad(to_hex(g+200000),64,'0'),'milesplit','100m','11.25','outdoor','recorded',false,false,
       'https://www.milesplit.com/meets/'||((g-1)%50000+1)||'/results/'||((g-1)%50000+1)||'/raw?retained='||repeat('a',160),
       jsonb_build_object('raw',jsonb_build_object('kind','raw_result','source_url','https://wi.milesplit.com/unmatched/'||g))
FROM generate_series(1,100000) g ORDER BY g;

ANALYZE schools;
ANALYZE athletes;
ANALYZE athlete_sources;
ANALYZE scrape_pages;
ANALYZE durable_page_operations;
ANALYZE durable_page_attempts;
ANALYZE durable_page_receipts;
ANALYZE results;
ANALYZE reviewed_result_links;
