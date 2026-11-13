SELECT EXISTS(SELECT 1 FROM scrape_pages p
 WHERE p.id=$1 AND p.source='milesplit' AND p.status='blocked' AND p.attempts=1
 AND p.url=$2 AND p.last_error=$3 AND p.cursor->>'access_stop_url'=$4
 AND (SELECT count(*) FROM durable_page_operations d WHERE d.page_id=p.id)=1
 AND (SELECT count(*) FROM durable_page_operations d JOIN durable_page_attempts a USING(operation_id) WHERE d.page_id=p.id)=1
 AND EXISTS(SELECT 1 FROM durable_page_operations d JOIN durable_page_attempts a USING(operation_id)
  WHERE d.page_id=p.id AND d.state='blocked' AND d.original_attempts=0
  AND d.cycle=1 AND d.cycle_attempts=1 AND a.state='failed' AND a.cycle=1
  AND a.ordinal=1 AND a.total_attempts=1 AND a.capture IS NULL AND a.parsed IS NULL
  AND d.outcome->>'state'='blocked' AND d.outcome->>'detail'=p.last_error AND a.outcome=d.outcome)
 AND NOT EXISTS(SELECT 1 FROM durable_page_operations d JOIN durable_wire_requests w USING(operation_id) WHERE d.page_id=p.id)
 AND NOT EXISTS(SELECT 1 FROM durable_page_receipts r WHERE r.page_id=p.id))
