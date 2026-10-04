from contextlib import contextmanager
import fcntl
import hashlib
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import uuid

import dev
import dev_checks as checks


def fingerprint(paths, root=None):
    root = dev.source_root(root)
    result = hashlib.sha256()
    for path in sorted(paths):
        result.update(str(path.relative_to(root)).encode() + b'\0')
        result.update(path.read_bytes() + b'\0')
    return result.hexdigest()


def components(root=None):
    root = dev.source_root(root)
    files = dev.source_files(root)
    python = root / 'apps/ytm-resolver'
    migrations = root / 'apps/core/migrations'
    extra = [root / 'docker/local.Dockerfile', root / 'docker/ytm-resolver.Dockerfile']
    return {'rust': fingerprint([path for path in files if not path.is_relative_to(python)], root),
            'python': fingerprint([path for path in files if path.is_relative_to(python)], root),
            'migrations': fingerprint([path for path in files if path.is_relative_to(migrations)], root),
            'dependencies': fingerprint([path for path in files if path.suffix in ('.toml', '.lock') or path.name == 'config'] + extra, root)}


def installed_components(profile):
    import dev_docker
    file = dev.builds_root(profile) / profile['build'] / 'build.json'
    document = json.loads(file.read_text())
    if not document.get('components'):
        if profile['build'] != dev_docker.identity(profile):
            raise RuntimeError('installed build lacks a source baseline; run just dev before watching')
        document['components'] = components()
        if profile['build'] != dev_docker.identity(profile):
            raise RuntimeError('source changed during baseline inspection; retry')
        dev.write_json(file, document)
    return document['components']


def editable(profile):
    if profile['name'] == 'personal':
        raise RuntimeError('personal uses just deploy; watch/migrate is for dev or named previews')


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


def replace(path, previous, candidate, schema_changed=False):
    import dev_docker
    return dev_docker.replace(path, previous, candidate, schema_changed)


def reload(path, profile, migrations=False):
    import dev_docker
    editable(profile)
    owner(path)
    current = components()
    previous_components = installed_components(profile) if not migrations else None
    if not migrations:
        for name in ('migrations', 'dependencies'):
            if current[name] != previous_components[name]:
                print(f'{name} changed; automatic replacement paused; run just dev {profile["name"]}', flush=True)
                return
    links_ready = all((path / name).is_symlink() for name in ('current-rust', 'current-resolver'))
    if not migrations and profile['build'] == dev_docker.identity(profile) and profile.get('resolver_snapshot') and links_ready:
        print('source unchanged; no restart', flush=True)
        return
    if migrations:
        dev_docker.start_db(path, profile)
        if migration_status(path, profile, display=False):
            raise RuntimeError('applied migration differs or is absent; restore the original file before migrating')
        dev.backup(path, profile)
    previous = dict(profile)
    candidate = dict(profile)
    dev_docker.build(path, candidate, persist=False)
    current = json.loads((dev.builds_root(candidate) / candidate['build'] / 'build.json').read_text())['components']
    if not migrations and any(current[name] != previous_components[name] for name in ('migrations', 'dependencies')):
        print(f'migrations/dependencies changed during build; replacement paused; run just dev {profile["name"]}', flush=True)
        return
    candidate['resolver_snapshot'] = True
    baseline = previous_components or json.loads((dev.builds_root(previous) / previous['build'] / 'build.json').read_text()).get('components', {})
    candidate['rust_build'] = previous.get('rust_build', previous['build']) if current['rust'] == baseline.get('rust') and previous.get('images', {}).get('compiler') == candidate.get('images', {}).get('compiler') else candidate['build']
    replace(path, previous, candidate, schema_changed=migrations and current['migrations'] != baseline.get('migrations'))


def watch_step(path, profile, token):
    import dev_docker
    editable(profile)
    if not token:
        raise RuntimeError('watch-step requires an active watcher session')
    owner(path, token)
    if not dev_docker.running(path, profile):
        print('profile stopped; save ignored; start it explicitly to resume', flush=True)
        return
    reload(path, profile)


def watch_command(profile, token):
    binary = shutil.which('watchexec')
    if not binary:
        raise RuntimeError('watchexec is missing; install it before using just dev')
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
            import dev_docker
            dev_docker.start(path, profile, profile['full'])
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


def migration_status(path, profile, display=True, root=None):
    root = dev.source_root(root)
    name, _ = dev.db_names(profile)
    container = dev.owned('container', name, profile)
    if not container or not container['State']['Running']:
        raise RuntimeError(f'database is stopped; run just up {profile["name"]} first')
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
    paths = sorted((root / 'apps/core/migrations').glob('*.sql'))
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
