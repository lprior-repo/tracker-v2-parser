SELECT p.url AS address,p.cursor,p.attempts,p.last_error AS detail,
 EXISTS(SELECT 1 FROM durable_page_operations d JOIN durable_page_attempts a USING(operation_id)
  WHERE d.page_id=p.id AND d.state='blocked' AND d.original_attempts=0
   AND d.cycle=1 AND d.cycle_attempts=1 AND a.state='failed' AND a.cycle=1
   AND a.ordinal=1 AND a.total_attempts=1 AND d.outcome->>'state'='blocked'
   AND d.outcome->>'detail'=p.last_error AND a.outcome=d.outcome) AS consistent_attempt,
 (SELECT count(*) FROM durable_page_operations d WHERE d.page_id=p.id) AS operations,
 (SELECT count(*) FROM durable_page_operations d JOIN durable_page_attempts a USING(operation_id) WHERE d.page_id=p.id) AS attempts_recorded,
 (SELECT count(*) FROM durable_page_operations d JOIN durable_wire_requests w USING(operation_id) WHERE d.page_id=p.id) AS wires,
 (SELECT count(*) FROM durable_page_operations d JOIN durable_wire_requests w USING(operation_id) WHERE d.page_id=p.id AND w.status=302 AND w.state='headers' AND w.origin=$1) AS redirects,
 EXISTS(SELECT 1 FROM durable_page_operations d JOIN durable_page_attempts a USING(operation_id) WHERE d.page_id=p.id AND (a.capture IS NOT NULL OR a.parsed IS NOT NULL))
 OR EXISTS(SELECT 1 FROM durable_page_receipts r WHERE r.page_id=p.id) AS captures,
 EXISTS(SELECT 1 FROM durable_wire_requests WHERE origin=$1 AND status IN(401,402,403,429,451)) AS origin_denied
FROM scrape_pages p WHERE p.status='blocked' AND p.cursor->>'kind'='contacts'
 AND EXISTS(SELECT 1 FROM unnest(ARRAY[p.url,p.cursor->>'access_stop_url']) stop(address)
  WHERE regexp_replace(lower(substring(stop.address from '(?i)^https?://([^/?#]+)')),CASE WHEN lower(stop.address) LIKE 'https://%' THEN ':443$' ELSE ':80$' END,'')=substring($1 from '^https://(.*)$'))
ORDER BY p.id LIMIT 1001
