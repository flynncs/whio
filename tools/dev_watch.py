from contextlib import contextmanager
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import shlex
import signal
import subprocess
import sys
import time
import uuid

import dev
import dev_checks as checks


def fingerprint(paths):
    result = hashlib.sha256()
    for path in sorted(paths):
        result.update(str(path.relative_to(dev.REPO)).encode() + b'\0')
        result.update(path.read_bytes() + b'\0')
    return result.hexdigest()


def components(profile=None):
    files = dev.source_files()
    python = dev.REPO / 'apps/ytm-resolver'
    migrations = dev.REPO / 'apps/core/migrations'
    extra = []
    if (profile or {}).get('runtime') == 'docker':
        extra = [dev.REPO / 'docker/local.Dockerfile', dev.REPO / 'docker/ytm-resolver.Dockerfile']
    return {'rust': fingerprint([path for path in files if not path.is_relative_to(python)]),
            'python': fingerprint([path for path in files if path.is_relative_to(python)]),
            'migrations': fingerprint([path for path in files if path.is_relative_to(migrations)]),
            'dependencies': fingerprint([path for path in files if path.suffix in ('.toml', '.lock') or path.name == 'config'] + extra)}


def installed_components(profile):
    file = dev.builds_root(profile) / profile['build'] / 'build.json'
    document = json.loads(file.read_text())
    if not document.get('components'):
        if profile['build'] != dev.current_identity(profile):
            raise RuntimeError('installed build lacks a source baseline; run just migrate before watching')
        document['components'] = components(profile)
        if profile['build'] != dev.current_identity(profile):
            raise RuntimeError('source changed during baseline inspection; retry')
        dev.write_json(file, document)
    return document['components']


def editable(profile):
    if profile['name'] == 'personal':
        raise RuntimeError('personal uses just upgrade; watch/migrate is for dev or named previews')


def owner(path, token=None):
    file = path / 'watcher.json'
    if not file.exists():
        if token:
            raise RuntimeError('watcher is no longer active')
        return
    document = json.loads(file.read_text())
    with (path / 'watcher.lock').open('a+') as stream:
        try:
            fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            if document['checkout'] != str(dev.REPO.resolve()) or token and token != document['token']:
                raise RuntimeError('profile watcher belongs to another checkout or session')
            return
    if token:
        raise RuntimeError('watcher owner has exited')


@contextmanager
def watcher_owner(path):
    file = path / 'watcher.lock'
    if file.is_symlink():
        raise RuntimeError('watcher lock must not be a symlink')
    with file.open('a+') as stream:
        try:
            fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise RuntimeError('a watcher already owns this profile')
        token = uuid.uuid4().hex
        dev.write_json(path / 'watcher.json', {'token': token, 'checkout': str(dev.REPO.resolve()), 'pid': os.getpid()})
        try:
            yield token
        finally:
            (path / 'watcher.json').unlink(missing_ok=True)


def process_states(path, profile):
    if profile.get('runtime') == 'docker':
        import dev_docker
        return dev_docker.process_states(path, profile)
    return {item['name']: item for item in json.loads(dev.supervisor(path, profile, 'process', 'list', '--output', 'json'))}


def wait_ready(profile, path=None, replaced=None):
    names = ('api', 'worker', 'youtube', 'metadata') if profile['full'] else ('api',)
    deadline = time.monotonic() + 45
    while time.monotonic() < deadline:
        states = process_states(path, profile) if path and replaced else {}
        changed = all(states.get(name, {}).get('is_running') and states[name].get('pid') != pid
                      for name, pid in (replaced or {}).items())
        if changed and all(dev.ready(profile['ports'][name]) for name in names):
            return
        time.sleep(0.2)
    raise RuntimeError('replacement did not become ready within 45 seconds')


def validate_resolvers(path, profile):
    if profile.get('runtime') == 'docker':
        import dev_docker
        return dev_docker.validate_resolvers(path, profile)
    root = dev.state_root() / 'builds' / profile['build'] / 'resolver/app'
    for file in root.rglob('*.py'):
        try:
            compile(file.read_bytes(), str(file), 'exec')
        except SyntaxError as error:
            raise RuntimeError(f'resolver syntax error in {file.relative_to(root)}:{error.lineno}; previous server retained')
    if not profile['full']:
        return
    configuration = json.loads((path / 'process-compose.json').read_text())
    for name, module in (('youtube', 'app.main'), ('metadata', 'app.metadata_main')):
        process = configuration['processes'][name]
        python = shlex.split(process['command'])[0]
        environment = dev.clean_environment()
        environment.update(dict(item.split('=', 1) for item in process['environment']))
        environment['PYTHONDONTWRITEBYTECODE'] = '1'
        log = path / 'logs' / f'{name}-import.log'
        with log.open('wb') as output:
            log.chmod(0o600)
            result = subprocess.run([python, '-c', f'import {module}'], cwd=process['working_dir'],
                                    env=environment, stdout=output, stderr=subprocess.STDOUT, timeout=20)
        if result.returncode:
            raise RuntimeError(f'{name} resolver import failed; previous server retained; inspect {log}')


def replace(path, previous, candidate, schema_changed=False):
    if candidate.get('runtime') == 'docker':
        import dev_docker
        return dev_docker.replace(path, previous, candidate, schema_changed)
    file = path / 'process-compose.json'
    original = file.read_bytes() if file.exists() else None
    running = dev.socket_path(previous).exists()
    previous_states = process_states(path, previous) if running else {}
    if running and not dev.ready(previous['ports']['api']):
        raise RuntimeError('current API is not ready; inspect status before replacing it')
    try:
        dev.config(path, candidate, candidate['full'])
        validate_resolvers(path, candidate)
    except BaseException:
        if original is not None:
            file.write_bytes(original)
        dev.runtime_links(path, previous)
        raise
    attempted = False
    configured = json.loads(file.read_text())['processes']
    old_configured = json.loads(original)['processes'] if original else {}
    replaced = {name: previous_states.get(name, {}).get('pid') for name, process in configured.items()
                if process != old_configured.get(name)}
    builds = dev.state_root() / 'builds'
    prior_components = json.loads((builds / previous['build'] / 'build.json').read_text()).get('components', {})
    candidate_components = json.loads((builds / candidate['build'] / 'build.json').read_text())['components']
    changed = []
    if prior_components.get('rust') != candidate_components['rust']:
        changed.extend(('worker', 'api') if candidate['full'] else ('api',))
    if candidate['full'] and prior_components.get('python') != candidate_components['python']:
        changed.extend(('youtube', 'metadata'))
    if not replaced:
        replaced = {name: previous_states.get(name, {}).get('pid') for name in changed}
    try:
        attempted = True
        if running:
            if configured != old_configured:
                dev.stop(path, previous, database=False)
                dev.start(path, candidate, candidate['full'], preserve_database=True)
            else:
                for name in changed:
                    dev.supervisor(path, candidate, 'process', 'restart', name)
                wait_ready(candidate, path, replaced)
        else:
            dev.start(path, candidate, candidate['full'], preserve_database=True)
        dev.write_json(path / 'profile.json', candidate)
        dev.create_user(path, candidate)
    except BaseException:
        if original is not None:
            file.write_bytes(original)
        dev.runtime_links(path, previous)
        dev.write_json(path / 'profile.json', previous)
        if attempted and running and not schema_changed:
            try:
                if configured != old_configured:
                    dev.stop(path, previous, database=False)
                    dev.start(path, previous, previous['full'], preserve_database=True)
                else:
                    for name in changed:
                        dev.supervisor(path, previous, 'process', 'restart', name)
                wait_ready(previous)
            except BaseException as recovery:
                raise RuntimeError(f'replacement and previous-server recovery failed: {recovery}; inspect profile logs')
        if schema_changed:
            raise RuntimeError('migration/startup failed; backup retained; inspect logs and recover explicitly')
        raise
    dev.prune_installed_builds(dev.state_root())
    print(f'{candidate["name"]} updated; Postgres retained; http://127.0.0.1:{candidate["ports"]["api"]}', flush=True)


def reload(path, profile, migrations=False):
    editable(profile)
    owner(path)
    current = components(profile)
    previous_components = installed_components(profile) if not migrations else None
    if not migrations:
        for name in ('migrations', 'dependencies'):
            if current[name] != previous_components[name]:
                print(f'{name} changed; automatic replacement paused; run just migrate {profile["name"]}', flush=True)
                return
    links_ready = all((path / name).is_symlink() for name in ('current-rust', 'current-resolver'))
    configuration = path / 'process-compose.json'
    if links_ready and configuration.exists():
        processes = json.loads(configuration.read_text())['processes']
        links_ready = shlex.split(processes['api']['command']) == [str(path / 'current-rust/whio-api')]
    else:
        links_ready = False
    if profile.get('runtime') == 'docker':
        links_ready = all((path / name).is_symlink() for name in ('current-rust', 'current-resolver'))
    if not migrations and profile['build'] == dev.current_identity(profile) and profile.get('resolver_snapshot') and links_ready:
        print('source unchanged; no restart', flush=True)
        return
    if migrations:
        dev.start_db(path, profile)
        if migration_status(path, profile, display=False):
            raise RuntimeError('applied migration differs or is absent; restore the original file before migrating')
        dev.backup(path, profile)
    previous = dict(profile)
    candidate = dict(profile)
    dev.build(path, candidate, persist=False)
    current = json.loads((dev.builds_root(candidate) / candidate['build'] / 'build.json').read_text())['components']
    if not migrations and any(current[name] != previous_components[name] for name in ('migrations', 'dependencies')):
        print(f'migrations/dependencies changed during build; replacement paused; run just migrate {profile["name"]}', flush=True)
        return
    candidate['resolver_snapshot'] = True
    baseline = previous_components or json.loads((dev.builds_root(previous) / previous['build'] / 'build.json').read_text()).get('components', {})
    candidate['rust_build'] = previous.get('rust_build', previous['build']) if current['rust'] == baseline.get('rust') and previous.get('images', {}).get('compiler') == candidate.get('images', {}).get('compiler') else candidate['build']
    replace(path, previous, candidate, schema_changed=migrations and current['migrations'] != baseline.get('migrations'))


def watch_step(path, profile, token):
    editable(profile)
    if not token:
        raise RuntimeError('watch-step requires an active watcher session')
    owner(path, token)
    if not dev.running(path, profile):
        print('profile stopped; save ignored; start it explicitly to resume', flush=True)
        return
    reload(path, profile)


def watch_command(profile, token):
    binary = shutil.which('watchexec')
    if not binary:
        raise RuntimeError('watchexec is missing; install it before using just watch')
    command = [binary, '--shell=none', '--project-origin', str(dev.REPO),
               '--workdir', str(dev.REPO), '--debounce', '500ms', '--on-busy-update', 'queue',
               '--stop-timeout', '15s', '--no-follow-symlinks']
    for directory in ('core', 'resolver-client-generated', 'subsonic-types-generated'):
        base = dev.REPO / 'apps' / directory
        command.extend(('--watch', str(base / 'src'), '--watch-non-recursive', str(base)))
    command.extend(('--watch', str(dev.REPO / 'apps/core/migrations'),
                    '--watch', str(dev.REPO / 'apps/ytm-resolver/app'),
                    '--watch-non-recursive', str(dev.REPO / 'apps/ytm-resolver'),
                    '--watch-non-recursive', str(dev.REPO)))
    if (dev.REPO / '.cargo').is_dir():
        command.extend(('--watch', str(dev.REPO / '.cargo')))
    if profile.get('runtime') == 'docker':
        command.extend(('--watch', str(dev.REPO / 'docker')))
    for pattern in ('**/target/**', '**/.venv/**', '**/__pycache__/**', '**/docs/**', '**/logs/**', '**/tests/**'):
        command.extend(('--ignore', pattern))
    command.extend(('--', sys.executable, str(dev.REPO / 'tools/dev.py'), 'watch-step', profile['name'], '--watch-token', token))
    return command


def watch(name):
    process = None
    with dev.state_lock():
        path, profile = dev.load_profile(name)
        editable(profile)
        watch_command(profile, 'probe')
    with watcher_owner(path) as token:
        with dev.state_lock():
            path, profile = dev.load_profile(name)
            installed_components(profile)
            reload(path, profile)
            path, profile = dev.load_profile(name)
            dev.start(path, profile, profile['full'])
        try:
            process = subprocess.Popen(watch_command(profile, token), env=dev.clean_environment(), start_new_session=True)
            print(f'watching {dev.REPO} for {name}; Ctrl-C stops watching and retains the server', flush=True)
            return process.wait()
        finally:
            if process is not None:
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGTERM)
                    try:
                        process.wait(timeout=20)
                    except subprocess.TimeoutExpired:
                        checks.stop_process_group(process)


def migration_status(path, profile, display=True):
    name, _ = dev.db_names(profile)
    container = dev.owned('container', name, profile)
    if not container or not container['State']['Running']:
        raise RuntimeError(f'database is stopped; run just start {profile["name"]} first')
    query = "SELECT coalesce(json_agg(row_to_json(m)), '[]') FROM (SELECT version, success, encode(checksum, 'hex') AS checksum FROM _sqlx_migrations ORDER BY version) m"
    probe = subprocess.run([*dev.docker_args(profile), 'exec', name, 'psql', '-U', 'whio', '-d', 'whio', '-tAc',
                            "SELECT to_regclass('_sqlx_migrations') IS NOT NULL"],
                           capture_output=True, text=True, timeout=15)
    if probe.returncode:
        raise RuntimeError('cannot inspect migration table; inspect database readiness')
    result = subprocess.run([*dev.docker_args(profile), 'exec', name, 'psql', '-U', 'whio', '-d', 'whio', '-tAc', query],
                            capture_output=True, text=True, timeout=15)
    if result.returncode and probe.stdout.strip() == 't':
        raise RuntimeError('cannot read SQLx migration state; inspect database readiness')
    applied = {int(item['version']): item for item in json.loads(result.stdout)} if probe.stdout.strip() == 't' else {}
    paths = sorted((dev.REPO / 'apps/core/migrations').glob('*.sql'))
    blocked = False
    for file in paths:
        match = re.fullmatch(r'(\d+)_(.+)\.sql', file.name)
        if not match:
            raise RuntimeError(f'unsupported migration filename: {file.name}')
        version = int(match[1])
        record = applied.pop(version, None)
        status = 'pending' if record is None else 'applied'
        if record and (not record['success'] or record['checksum'] != hashlib.sha384(file.read_bytes()).hexdigest()):
            status = 'changed/failed; do not edit an applied migration'
            blocked = True
        if display or status.startswith('changed'):
            print(f'{status}: {file.name}')
    for version in applied:
        print(f'absent from this checkout: applied migration {version}')
        blocked = True
    if display:
        print('SQLx files shown; Awa migrations run through existing app startup', flush=True)
    return blocked
