import json
import subprocess

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
    first = cli(runtime, ['crawl', '--config', str(path), '--generation',
        'cli-' + identity, '--hours', '1', '--max-batches', '1'])
    invocation = first['submission']['invocationId']
    request = runtime.args.evidence / 'workflow-cli-request.json'
    save(request, first['request'])
    second = cli(runtime, ['submit', '--request', str(request)])
    if second['invocationId'] != invocation:
        raise RuntimeError('workflow key did not deduplicate native CLI submissions')
    report = output(runtime.ingress, invocation, field='progress')
    if report['progress']['selected'] != 0 or report['continuation'] is not None:
        raise RuntimeError('empty focused workflow must not acquire source pages or continue')
    if report['stopped_reason'] != 'bounded batch count reached; retained checkpoints are resumable':
        raise RuntimeError('native workflow did not complete its bounded run')
    status = cli(runtime, ['status', '--key', first['workflow_key']])
    if status != report:
        raise RuntimeError('native workflow status differs from its completed report')
    wires = sql(runtime.database, 'SELECT count(*) FROM durable_wire_requests;')
    if wires != 0:
        raise RuntimeError('empty focused workflow attempted a source request')
    result = dict(invocation_id=invocation, first=first, duplicate=second, report=report,
                  native_start_submit_and_status='passed', public_source_requests=wires,
                  service_export_invocation=service['invocationId'],
                  duplicate_service_export=duplicate_service, service_export=service_output)
    save(runtime.args.evidence / 'workflow-cli-proof.json', result)
    return result
