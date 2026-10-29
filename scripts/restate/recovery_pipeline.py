import json
import re
import time

from recovery_cli import cli
from recovery_evidence import domain
from recovery_support import http, output, save, sha256, sql


def config_path(runtime):
    path = runtime.args.evidence / 'pipeline-focused-config.toml'
    path.write_text('''states = ["CO"]
graduation_years = [2027]
season = 2026
[limits]
requests_per_second = 2.0
max_pages = 5
max_attempts = 3
concurrency = 3
[sources.athleticnet]
enabled = false
transport = "http"
[sources.milesplit]
enabled = true
transport = "http"
[export]
layout = "single_sheet"
rows_per_file = 2000
per_athlete_sheets = false
[acquisition]
mode = "public_results"
profile_coverage_only = true
collect_contacts = false
''')
    return path


def seed(runtime):
    identifiers = []
    for number in range(942700, 942707):
        target = dict(source='milesplit', kind='team',
            url='https://co.milesplit.com/teams/' + str(number) + '-owned-pipeline-fixture',
            context=dict(state='CO', school_id=str(number), season=2026))
        cursor = json.dumps(target)
        key = sha256(cursor.encode())
        escaped = cursor.replace("'", "''")
        identifier = sql(runtime.database, "INSERT INTO scrape_pages(source,page_key,url,cursor,status,attempts,last_error) "
            "VALUES('milesplit','" + key + "','" + target['url'] + "','" + escaped
            + "'::jsonb,'failed',3,'unsupported fixture format') RETURNING id;")
        identifiers.append(identifier)
    return identifiers


def checkpoints(runtime, identifiers):
    if len(identifiers) != 7 or any(not isinstance(identity, int) or identity <= 0 for identity in identifiers):
        raise RuntimeError('pipeline proof requires seven exact positive checkpoint IDs')
    ids = ','.join(str(identity) for identity in identifiers)
    return sql(runtime.database,
        'SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM scrape_pages p WHERE id IN (' + ids + ');')


def query(runtime, statement, label):
    status, value = http(runtime.admin, '/query', {'query': statement})
    save(runtime.args.evidence / (label + '.json'), dict(status=status, response=value))
    if status != 200 or not isinstance(value, dict) or not isinstance(value.get('rows'), list):
        raise RuntimeError('pipeline proof requires readable native Restate query evidence: ' + label)
    return value['rows']


def journal(runtime, invocation, label):
    if not re.fullmatch(r'inv_[A-Za-z0-9]+', invocation):
        raise RuntimeError('pipeline journal requires canonical invocation ID')
    return query(runtime, "SELECT * FROM sys_journal WHERE id='" + invocation + "' ORDER BY index", label)


def entry(row):
    value = row.get('entry_json')
    return json.loads(value) if isinstance(value, str) else value


def invocation_ids(value):
    if isinstance(value, dict):
        return {identity for child in value.values() for identity in invocation_ids(child)}
    if isinstance(value, list):
        return {identity for child in value for identity in invocation_ids(child)}
    if isinstance(value, str):
        return set(re.findall(r'inv_[A-Za-z0-9]+', value))
    return set()


def validate_root_journal(rows, child_ids, count):
    ordered = sorted(rows, key=lambda row: row['index'])
    names = [row.get('name') for row in ordered if row.get('entry_type') == 'Command: Run']
    for index in range(count):
        for prefix in ('select-page-', 'reserve-page-'):
            if names.count(prefix + format(index, '06')) != 1:
                raise RuntimeError('pipeline root must journal each select/reserve exactly once')
    sends = [row for row in ordered if 'onewaycall' in row.get('entry_type', '').lower()
        and 'AthleticPage' in json.dumps(entry(row))]
    if len(sends) != count:
        raise RuntimeError('pipeline root must eagerly send each selected page exactly once')
    send_ids = []
    for row in sends:
        matched = invocation_ids(entry(row)) & set(child_ids)
        if len(matched) != 1:
            raise RuntimeError('pipeline send journal cannot identify its exact child invocation')
        send_ids.append(next(iter(matched)))
    attaches = []
    attach_indices = []
    for row in ordered:
        if 'attachinvocation' not in row.get('entry_type', '').lower():
            continue
        matched = invocation_ids(entry(row)) & set(child_ids)
        if len(matched) == 1:
            attaches.append(next(iter(matched)))
            attach_indices.append(row['index'])
    if send_ids != child_ids or attaches != child_ids:
        raise RuntimeError('pipeline sends/ordered attaches do not match the seven unique page children')
    for start in range(0, count, 3):
        finish = min(start + 3, count)
        if max(row['index'] for row in sends[start:finish]) >= min(attach_indices[start:finish]):
            raise RuntimeError('pipeline window attached before all of its child sends were journaled')
    return dict(selected=count, sent_child_ids=send_ids, ordered_attached_child_ids=attaches,
        bounded_window=3, window_sends_precede_attaches=True)


def verify_child(runtime, child, expected_page):
    rows = journal(runtime, child['id'], 'pipeline-child-' + str(expected_page))
    inputs = [entry(row) for row in rows if row.get('entry_type') == 'Command: Input']
    if len(inputs) != 1:
        raise RuntimeError('pipeline child must retain exactly one invocation input')
    execution = json.loads(bytes(inputs[0]['Command']['Input']['payload']))
    if execution['request']['binding']['page_id'] != expected_page or execution['config']['limits']['concurrency'] != 3:
        raise RuntimeError('pipeline child input does not match its exact reserved checkpoint')
    outcome = output(runtime.ingress, child['id'], field='state')
    if outcome.get('state') != 'rejected':
        raise RuntimeError('settled legacy pipeline child must return rejected without HTTP')
    return dict(invocation_id=child['id'], page_id=expected_page, outcome=outcome)


def prove_pipeline(runtime, identity):
    before_domain = domain(runtime.database)
    identifiers = seed(runtime)
    before = checkpoints(runtime, identifiers)
    generation = 'pipeline-' + identity
    first = cli(runtime, ['crawl', '--config', str(config_path(runtime)), '--generation',
        generation, '--hours', '1', '--max-batches', '2'])
    request = runtime.args.evidence / 'pipeline-request.json'
    save(request, first['request'])
    duplicate = cli(runtime, ['submit', '--request', str(request)])
    invocation = first['submission']['invocationId']
    if duplicate['invocationId'] != invocation:
        raise RuntimeError('pipeline workflow submissions did not deduplicate to the same invocation')
    report = output(runtime.ingress, invocation, field='progress')
    next_key = generation + '-000001'
    if (report['progress']['selected'], report['progress']['rejected'], report['continuation']) != (5, 5, next_key):
        raise RuntimeError('pipeline first batch did not select/reject exactly five checkpoints')
    until = time.monotonic() + 90
    continuation = None
    while time.monotonic() < until:
        continuation = cli(runtime, ['status', '--key', next_key])
        if isinstance(continuation, dict):
            break
        time.sleep(0.1)
    if not isinstance(continuation, dict) or (continuation['progress']['selected'], continuation['progress']['rejected']) != (2, 2):
        raise RuntimeError('pipeline continuation did not select/reject exactly the remaining two checkpoints')
    if continuation['continuation'] is not None or continuation['stopped_reason'] != 'bounded batch count reached; retained checkpoints are resumable':
        raise RuntimeError('pipeline continuation violated its bounded run')
    children = query(runtime, "SELECT id,idempotency_key,status FROM sys_invocation WHERE target_service_name='AthleticPage' AND idempotency_key LIKE '"
        + generation + "-%' ORDER BY idempotency_key", 'pipeline-children')
    keys = [first['workflow_key'] + '-page-' + format(index, '06') for index in range(5)]
    keys += [next_key + '-page-' + format(index, '06') for index in range(2)]
    if [child['idempotency_key'] for child in children] != keys or len({child['id'] for child in children}) != 7:
        raise RuntimeError('pipeline did not create exactly seven unique keyed page invocations')
    evidence = [verify_child(runtime, child, page) for child, page in zip(children, identifiers)]
    root_rows = journal(runtime, invocation, 'pipeline-root-journal')
    next_rows = query(runtime, "SELECT id FROM sys_invocation WHERE target='AthleticCrawl/" + next_key + "/run'", 'pipeline-continuation-invocation')
    if len(next_rows) != 1:
        raise RuntimeError('pipeline continuation invocation identity is not unique')
    next_journal = journal(runtime, next_rows[0]['id'], 'pipeline-continuation-journal')
    journals = [validate_root_journal(root_rows, [child['id'] for child in children[:5]], 5),
        validate_root_journal(next_journal, [child['id'] for child in children[5:]], 2)]
    operations = sql(runtime.database, "SELECT jsonb_agg(jsonb_build_object('page_id',page_id,'state',state,'original_attempts',original_attempts) ORDER BY page_id) FROM durable_page_operations WHERE run_generation='" + generation + "-schools';")
    if operations != [dict(page_id=page, state='rejected', original_attempts=3) for page in identifiers]:
        raise RuntimeError('pipeline did not retain exactly seven settled rejected operations')
    if checkpoints(runtime, identifiers) != before:
        raise RuntimeError('pipeline rewrote original checkpoint counters/errors or metadata')
    expected_domain = dict(before_domain)
    expected_domain['attempts'] += 21
    expected_domain['seeded_legacy_attempts'] += 21
    after_domain = domain(runtime.database)
    if after_domain != expected_domain or after_domain['source_wire_requests'] != 0:
        raise RuntimeError('pipeline changed domain facts or attempted a public source request')
    proof = dict(invocation_id=invocation, first=first, duplicate=duplicate, report=report,
        continuation=continuation, continuation_key=next_key, page_ids=identifiers,
        original_checkpoints=before, operations=operations, children=evidence,
        journals=journals, public_source_requests=0, checkpoint_counters_and_errors_unchanged=True)
    save(runtime.args.evidence / 'pipeline-proof.json', proof)
    return proof


def verify_pipeline_restore(runtime, proof):
    root = cli(runtime, ['status', '--key', proof['first']['workflow_key']])
    continuation = cli(runtime, ['status', '--key', proof['continuation_key']])
    if root != proof['report'] or continuation != proof['continuation']:
        raise RuntimeError('pipeline root/continuation reports changed after native backup restoration')
    if checkpoints(runtime, proof['page_ids']) != proof['original_checkpoints']:
        raise RuntimeError('pipeline checkpoint counters/errors changed after native backup restoration')
    if sql(runtime.database, 'SELECT count(*) FROM durable_wire_requests;') != 0:
        raise RuntimeError('restored pipeline issued a public source request')
    return dict(root_report_unchanged=True, continuation_report_unchanged=True,
        seven_checkpoint_records_unchanged=True, public_source_requests=0)
