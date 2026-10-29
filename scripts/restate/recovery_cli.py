import json
import subprocess
import time

from recovery_support import artifact, output, save, sql


def cli(runtime, arguments):
    response = subprocess.run([str(runtime.args.worker), 'durable', *arguments,
        '--ingress', runtime.ingress], env=dict(runtime.environment, DATABASE_URL=runtime.database),
        cwd=runtime.args.evidence, capture_output=True, text=True, check=False, timeout=60)
    if response.returncode:
        raise RuntimeError('native durable CLI failed: ' + response.stderr[-2048:])
    return json.loads(response.stdout)


def prove_workflow_cli(runtime, identity):
    path = runtime.args.evidence / 'empty-focused-config.toml'
    path.write_text('''states = ["MI"]
graduation_years = [2027]
season = 2026
[limits]
requests_per_second = 2.0
max_pages = 1
max_attempts = 3
concurrency = 1
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
    operation_id = 'cli-export-' + identity
    service = cli(runtime, ['export', '--config', str(path), '--operation-id', operation_id])
    duplicate_service = cli(runtime, ['export', '--config', str(path), '--operation-id', operation_id])
    if service['invocationId'] != duplicate_service['invocationId']:
        raise RuntimeError('native service CLI idempotency key did not deduplicate export')
    service_output = output(runtime.ingress, service['invocationId'])
    artifact(service_output['receipt'], runtime.root)
    target = dict(source='milesplit', kind='team',
        url='https://mi.milesplit.com/teams/942615-owned-workflow-fixture',
        context=dict(state='MI', school_id='942615', season=2026))
    cursor = json.dumps(target).replace("'", "''")
    legacy_id = sql(runtime.database, "INSERT INTO scrape_pages(source,page_key,url,cursor,status,attempts,last_error) "
        "VALUES('milesplit',repeat('f',64),'" + target['url'] + "','" + cursor
        + "'::jsonb,'failed',3,'unsupported fixture format') RETURNING id;")
    first = cli(runtime, ['crawl', '--config', str(path), '--generation',
        'cli-' + identity, '--hours', '1', '--max-batches', '2'])
    invocation = first['submission']['invocationId']
    request = runtime.args.evidence / 'workflow-cli-request.json'
    save(request, first['request'])
    second = cli(runtime, ['submit', '--request', str(request)])
    if second['invocationId'] != invocation:
        raise RuntimeError('workflow key did not deduplicate native CLI submissions')
    report = output(runtime.ingress, invocation, field='progress')
    next_key = 'cli-' + identity + '-000001'
    if (report['progress']['selected'], report['progress']['rejected'], report['continuation'], report['stopped_reason']) != (1, 1, next_key, None):
        raise RuntimeError('first workflow must preserve the legacy rejection and schedule its next batch')
    until = time.monotonic() + 90
    continuation = None
    while time.monotonic() < until:
        continuation = cli(runtime, ['status', '--key', next_key])
        if isinstance(continuation, dict):
            break
        time.sleep(0.1)
    if not isinstance(continuation, dict) or continuation['progress']['selected'] != 0:
        raise RuntimeError('durably scheduled continuation did not finish the empty queue')
    if continuation['continuation'] is not None or continuation['stopped_reason'] != 'bounded batch count reached; retained checkpoints are resumable':
        raise RuntimeError('native continuation did not respect the bounded run')
    status = cli(runtime, ['status', '--key', first['workflow_key']])
    if status != report:
        raise RuntimeError('native workflow status differs from its completed report')
    wires = sql(runtime.database, 'SELECT count(*) FROM durable_wire_requests;')
    if wires != 0:
        raise RuntimeError('empty focused workflow attempted a source request')
    legacy = sql(runtime.database, "SELECT jsonb_build_object('attempts',p.attempts,'status',p.status,"
        "'last_error',p.last_error,'operation',d.state) FROM scrape_pages p JOIN durable_page_operations d ON d.page_id=p.id WHERE p.id=" + str(legacy_id) + ';')
    if legacy != dict(attempts=3, status='failed', last_error='unsupported fixture format', operation='rejected'):
        raise RuntimeError('native continuation rewrote the original legacy checkpoint')
    result = dict(invocation_id=invocation, first=first, duplicate=second, report=report,
                  native_start_submit_and_status='passed', public_source_requests=wires,
                  continuation=continuation, continuation_key=next_key, legacy_checkpoint=legacy,
                  service_export_invocation=service['invocationId'],
                  duplicate_service_export=duplicate_service, service_export=service_output)
    save(runtime.args.evidence / 'workflow-cli-proof.json', result)
    return result


def verify_cli_restore(runtime, proof):
    root = cli(runtime, ['status', '--key', proof['first']['workflow_key']])
    continuation = cli(runtime, ['status', '--key', proof['continuation_key']])
    if root != proof['report'] or continuation != proof['continuation']:
        raise RuntimeError('workflow reports changed after native server backup restoration')
    return dict(workflow_report_unchanged=True, continuation_report_unchanged=True)
