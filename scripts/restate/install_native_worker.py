import argparse
import hashlib
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess


def private_json(path, value):
    with path.open('x') as handle:
        json.dump(value, handle, indent=2)
        handle.write('\n')
    path.chmod(0o600)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--runtime', type=Path, required=True)
    parser.add_argument('--secrets', type=Path, required=True)
    parser.add_argument('--port', type=int, default=9085)
    parser.add_argument('--activate', action='store_true')
    parser.add_argument('--published-workbook', type=Path)
    args = parser.parse_args()
    os.umask(0o077)
    if not 1024 <= args.port <= 65535:
        raise ValueError('worker port must be unprivileged')
    binary = args.binary.resolve(strict=True)
    secrets = args.secrets.resolve(strict=True)
    runtime = args.runtime.resolve()
    runtime.mkdir(parents=True, exist_ok=True, mode=0o700)
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    version = runtime / 'workers' / digest
    version.mkdir(parents=True, exist_ok=True, mode=0o700)
    deployed = version / 'track-scrapper-v2'
    if deployed.exists():
        if hashlib.sha256(deployed.read_bytes()).hexdigest() != digest:
            raise ValueError('existing immutable binary has different bytes')
    else:
        shutil.copy2(binary, deployed)
        deployed.chmod(0o700)
    artifacts = runtime / 'artifacts'
    artifacts.mkdir(exist_ok=True, mode=0o700)
    logs = version / 'logs'
    logs.mkdir(exist_ok=True, mode=0o700)
    configuration = {
        'binary': str(deployed), 'binary_sha256': digest,
        'secrets': str(secrets), 'artifacts': str(artifacts),
        'bind': f'127.0.0.1:{args.port}',
        'published_workbook': str(args.published_workbook.absolute()) if args.published_workbook else None,
    }
    config_path = version / 'worker.json'
    if config_path.exists():
        if json.loads(config_path.read_text()) != configuration:
            raise ValueError('immutable worker configuration differs')
    else:
        private_json(config_path, configuration)
    launcher = version / 'launch.py'
    body = '''import hashlib, json, os
from pathlib import Path
os.umask(0o077)
configuration = json.loads(Path(__file__).with_name('worker.json').read_text())
binary = configuration['binary']
if hashlib.sha256(Path(binary).read_bytes()).hexdigest() != configuration['binary_sha256']:
    raise RuntimeError('immutable worker binary digest mismatch')
environment = dict(os.environ)
for line in Path(configuration['secrets']).read_text().splitlines():
    key, value = line.split('=', 1)
    environment[key] = json.loads(value)
if not environment.get('DATABASE_URL'):
    raise RuntimeError('DATABASE_URL missing from private runtime environment')
arguments = [binary, 'durable', 'serve', '--bind', configuration['bind'],
             '--artifacts', configuration['artifacts']]
if configuration.get('published_workbook'):
    arguments.extend(['--published-workbook', configuration['published_workbook']])
os.execve(binary, arguments, environment)
'''
    if launcher.exists() and launcher.read_text() != body:
        raise ValueError('existing immutable launcher differs')
    launcher.write_text(body)
    launcher.chmod(0o600)
    label = f'local.scraperv2native.worker.{digest[:12]}'
    agents = Path.home() / 'Library' / 'LaunchAgents'
    agents.mkdir(exist_ok=True)
    plist = agents / (label + '.plist')
    value = {
        'Label': label,
        'ProgramArguments': [shutil.which('python3'), str(launcher)],
        'WorkingDirectory': str(version),
        'EnvironmentVariables': {'PATH': os.environ.get('PATH', '/usr/bin:/bin'), 'RUST_LOG': 'info'},
        'RunAtLoad': True, 'KeepAlive': True, 'ThrottleInterval': 10,
        'ExitTimeOut': 120, 'Umask': 0o077,
        'StandardOutPath': str(logs / 'stdout.log'),
        'StandardErrorPath': str(logs / 'stderr.log'),
    }
    if plist.exists() and plistlib.loads(plist.read_bytes()) != value:
        raise ValueError('existing deployment LaunchAgent differs')
    plist.write_bytes(plistlib.dumps(value))
    plist.chmod(0o600)
    receipt = {**configuration, 'label': label, 'plist': str(plist), 'activated': False}
    if args.activate:
        subprocess.run(['launchctl', 'bootstrap', f'gui/{os.getuid()}', str(plist)], check=True)
        receipt['activated'] = True
    print(json.dumps(receipt, indent=2))


if __name__ == '__main__':
    main()
