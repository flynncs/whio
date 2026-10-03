import argparse
import base64
from contextlib import contextmanager
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import shlex
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
import urllib.parse
import uuid

import dev_checks as checks
import dev_media


REPO = Path(__file__).resolve().parent.parent
STATE = Path(os.environ.get('LUMEN_DEV_STATE', str(Path.home() / (
    'Library/Application Support/lumen-dev' if sys.platform == 'darwin' else '.local/state/lumen-dev')))).expanduser().absolute()
BINARIES = ('whio-api', 'whio-cli', 'whio-worker')


def write_json(path, value):
    temporary = path.with_name(path.name + '.new')
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(descriptor, 'w') as stream:
        json.dump(value, stream, indent=2)
        stream.write('\n')
    temporary.replace(path)


def state_root():
    if STATE.is_symlink():
        raise RuntimeError('state root must not be a symlink')
    STATE.mkdir(parents=True, mode=0o700, exist_ok=True)
    if STATE.stat().st_uid != os.getuid():
        raise RuntimeError('state root must belong to the current user')
    marker = STATE / 'owner.json'
    expected = {'kind': 'lumen-dev', 'version': 1, 'uid': os.getuid()}
    if marker.exists():
        if json.loads(marker.read_text()) != expected:
            raise RuntimeError('state root has a different owner or format')
    elif any(STATE.iterdir()):
        raise RuntimeError('refusing to adopt an existing nonempty state root')
    else:
        write_json(marker, expected)
    STATE.chmod(0o700)
    for name in ('profiles', 'builds'):
        path = STATE / name
        if path.is_symlink():
            raise RuntimeError('state subdirectories must not be symlinks')
        path.mkdir(mode=0o700, exist_ok=True)
    return STATE


def profile_name(value):
    if value not in ('personal', 'dev') and not re.fullmatch(r'preview-[a-z0-9][a-z0-9-]{0,31}', value):
        raise ValueError('profile must be personal, dev or preview-<task-name>')
    return value


@contextmanager
def state_lock():
    path = state_root() / 'operation.lock'
    if path.is_symlink():
        raise RuntimeError('state lock must not be a symlink')
    with path.open('a+') as stream:
        try:
            fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise RuntimeError('another dev setup operation is active; retry after it finishes')
        yield


def free_port():
    with socket.socket() as probe:
        probe.bind(('127.0.0.1', 0))
        return probe.getsockname()[1]


def check_ports(ports):
    if len(set(ports.values())) != len(ports):
        raise RuntimeError('profile ports must be distinct')
    for name, port in ports.items():
        try:
            with socket.socket() as probe:
                probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                probe.bind(('127.0.0.1', port))
                probe.listen()
        except OSError:
            raise RuntimeError(f'{name} port {port} is occupied; its owner was left running')


def load_profile(name, create=False):
    profile_name(name)
    root = state_root()
    path = root / 'profiles' / name
    if path.is_symlink():
        raise RuntimeError('profile directory must not be a symlink')
    file = path / 'profile.json'
    if file.exists():
        value = json.loads(file.read_text())
        if value['name'] != name or value['uid'] != os.getuid():
            raise RuntimeError('profile owner differs')
        return path, value
    if not create:
        raise RuntimeError(f'{name} is not set up; run just setup {name}')
    if path.exists() and any(path.iterdir()):
        raise RuntimeError('refusing to adopt a nonempty profile directory')
    path.mkdir(mode=0o700, exist_ok=True)
    ports = dict(zip(('api', 'worker', 'youtube', 'metadata', 'postgres'),
                     (3000, 3001, 8000, 8100, 5432) if name == 'personal' else
                     (3100, 3101, 18000, 18100, 55432) if name == 'dev' else
                     tuple(free_port() for _ in range(5))))
    check_ports(ports)
    value = {'name': name, 'uid': os.getuid(), 'ports': ports, 'created': time.time(),
             'expires': time.time() + 86400 if name.startswith('preview-') else None,
             'credential_key': base64.b64encode(os.urandom(32)).decode(),
             'postgres_password': secrets.token_hex(24), 'build': None, 'full': False, 'runtime': 'docker'}
    write_json(file, value)
    for directory in ('logs', 'artwork', 'spool', 'backups'):
        (path / directory).mkdir(mode=0o700, exist_ok=True)
    return path, value


def source_files():
    files = []
    for directory in ('core', 'resolver-client-generated', 'subsonic-types-generated', 'ytm-resolver'):
        base = REPO / 'apps' / directory
        for parent, directories, names in os.walk(base):
            directories[:] = [name for name in directories if name not in
                              ('target', '.venv', '__pycache__', '.pytest_cache', '.ruff_cache')]
            for name in names:
                path = Path(parent) / name
                if path.suffix in ('.rs', '.sql', '.py', '.toml', '.lock') or name == 'README.md' or path.is_relative_to(base / 'tests/fixtures'):
                    if path.is_symlink():
                        raise RuntimeError('build source must not be a symlink')
                    files.append(path)
    for path in (REPO / 'rust-toolchain.toml', REPO / '.cargo/config.toml', REPO / '.cargo/config'):
        if path.exists():
            if path.is_symlink():
                raise RuntimeError('build source must not be a symlink')
            files.append(path)
    return sorted(files)


def source_identity():
    digest = hashlib.sha256()
    for path in source_files():
        digest.update(str(path.relative_to(REPO)).encode() + b'\0')
        digest.update(path.read_bytes())
        digest.update(b'\0')
    return digest.hexdigest()


def builds_root(profile):
    if profile.get('runtime') != 'docker':
        return state_root() / 'builds'
    directory = state_root() / 'container-builds' / profile['architecture']
    if directory.is_symlink() or directory.parent.is_symlink():
        raise RuntimeError('container build directory must not be a symlink')
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    return directory


def current_identity(profile):
    if profile.get('runtime') == 'docker':
        import dev_docker
        return dev_docker.identity(profile)
    return source_identity()


def running(path, profile):
    if profile.get('runtime') == 'docker':
        import dev_docker
        return dev_docker.running(path, profile)
    return socket_path(profile).exists()


def docker_args(profile=None):
    context = (profile or {}).get('docker_context')
    return ['docker', '--context', context] if context else ['docker']


def prune_installed_builds(root):
    referenced = set()
    for file in (root / 'profiles').glob('*/profile.json'):
        value = json.loads(file.read_text())
        if not value.get('build'):
            continue
        base = builds_root(value)
        referenced.update(base / identity for identity in (value['build'], value.get('rust_build')) if identity)
    for file in (root / 'profiles').glob('*/backups/*/backup.json'):
        value = json.loads(file.read_text())
        saved_file = file.parent / 'profile.json'
        saved = json.loads(saved_file.read_text()) if saved_file.exists() else {}
        base = builds_root(saved)
        referenced.update(base / identity for identity in (value['build'], value.get('rust_build')) if identity)
    directories = list((root / 'builds').iterdir())
    if (root / 'container-builds').is_dir():
        directories.extend((root / 'container-builds').glob('*/*'))
    for directory in directories:
        if not re.fullmatch(r'[a-f0-9]{64}', directory.name) or directory.is_symlink():
            continue
        manifest = directory / 'build.json'
        if not manifest.is_file() or json.loads(manifest.read_text()).get('identity', json.loads(manifest.read_text()).get('source_sha256')) != directory.name:
            continue
        if directory not in referenced:
            shutil.rmtree(directory)
            print(f'removed unused installed build {directory.name[:12]}')


def build(path, profile, persist=True):
    if profile.get('runtime') == 'docker':
        import dev_docker
        return dev_docker.build(path, profile, persist)
    import dev_watch
    identity = source_identity()
    components = dev_watch.components()
    root = state_root()
    destination = root / 'builds' / identity
    if not (destination / 'build.json').is_file():
        cache = checks.runtime_root()
        key = checks.cache_key(REPO)
        with checks.admission(cache, key, 60) as (_, descriptors):
            checks.prune_caches(cache)
            checks.check_storage(cache)
            target = cache / 'targets' / key
            target.mkdir(exist_ok=True)
            environment = os.environ.copy()
            environment.update({'CARGO_TARGET_DIR': str(target), 'CARGO_BUILD_JOBS': '2',
                                'CARGO_INCREMENTAL': '0', 'CARGO_PROFILE_DEV_DEBUG': '0',
                                'CARGO_PROFILE_DEV_STRIP': 'none'})
            for name in ('DATABASE_URL', 'WHIO_DATABASE_URL', 'RUSTC_WRAPPER'):
                environment.pop(name, None)
            log = cache / 'logs' / f'{key}-build-{uuid.uuid4().hex[:8]}.log'
            command = ['cargo', 'build', '--locked', '--color', 'never', '--manifest-path',
                       str(REPO / 'apps/core/Cargo.toml')]
            for name in BINARIES:
                command.extend(('--bin', name))
            previous = root / 'builds' / (profile.get('build') or 'absent')
            previous_manifest = previous / 'build.json'
            reuse = previous_manifest.is_file() and json.loads(previous_manifest.read_text()).get('components', {}).get('rust') == components['rust']
            if reuse:
                print('Rust source unchanged; reusing installed binaries', flush=True)
                code, elapsed = 0, 0
            else:
                print(f'building current source {identity[:12]}; two compiler jobs; log {log}', flush=True)
                code, _, elapsed = checks.run_tests(command, environment, log, 900,
                                                    lambda: checks.check_storage(cache), pass_fds=descriptors)
            os.utime(target)
            if code:
                raise RuntimeError(f'build failed ({code}); previous installed build retained')
            if source_identity() != identity:
                raise RuntimeError('source changed while building; previous installed build retained; retry')
            staging = root / 'builds' / f'pending-{uuid.uuid4().hex}'
            staging.mkdir(mode=0o700)
            try:
                for name in BINARIES:
                    shutil.copy2(previous / name if reuse else target / 'debug' / name, staging / name)
                python = staging / 'resolver'
                python.mkdir()
                source = REPO / 'apps/ytm-resolver'
                for name in ('pyproject.toml', 'uv.lock', 'README.md'):
                    shutil.copy2(source / name, python / name)
                shutil.copytree(source / 'app', python / 'app', ignore=shutil.ignore_patterns('__pycache__', '*.pyc'))
                if source_identity() != identity:
                    raise RuntimeError('source changed while installing; previous installed build retained; retry')
                write_json(staging / 'build.json', {'source_sha256': identity,
                           'head': subprocess.check_output(['git', '-C', str(REPO), 'rev-parse', 'HEAD'], text=True).strip(),
                           'created': time.time(), 'seconds': elapsed, 'source_checkout': str(REPO),
                           'components': components})
                staging.rename(destination)
            finally:
                if staging.exists():
                    shutil.rmtree(staging)
    profile['build'] = identity
    manifest = destination / 'build.json'
    document = json.loads(manifest.read_text())
    if not document.get('components'):
        if source_identity() != identity:
            raise RuntimeError('source changed while inspecting installed build; retry')
        document['components'] = components
        write_json(manifest, document)
    if persist:
        write_json(path / 'profile.json', profile)
        prune_installed_builds(root)
    print(f'installed build {identity[:12]} for {profile["name"]}', flush=True)


def label(profile):
    return f'{os.getuid()}-{profile["name"]}'


def db_names(profile):
    if profile.get('database_container'):
        return profile['database_container'], profile['database_volume']
    name = f'lumen-local-{label(profile)}'
    return name, name + '-pg'


def inspect(kind, name, profile=None):
    result = subprocess.run([*docker_args(profile), kind, 'inspect', name], capture_output=True, text=True, timeout=30)
    if result.returncode:
        if f'no such {kind}' in result.stderr.lower():
            return None
        raise RuntimeError(f'cannot inspect Docker {kind}; check OrbStack')
    return json.loads(result.stdout)[0]


def owned(kind, name, profile):
    value = inspect(kind, name, profile)
    if value is None:
        return None
    labels = value.get('Labels') if kind == 'volume' else value.get('Config', {}).get('Labels')
    if (labels or {}).get('dev.lumen.local-profile') != label(profile):
        raise RuntimeError(f'{kind} {name} has another owner; left untouched')
    return value


def docker_operation(*arguments, profile=None):
    result = subprocess.run([*docker_args(profile), *arguments], capture_output=True, text=True, timeout=30)
    if result.returncode:
        raise RuntimeError('owned Docker operation failed; check OrbStack and profile status')
    return result.stdout.strip()


def start_db(path, profile):
    if profile.get('runtime') == 'docker':
        import dev_docker
        return dev_docker.start_db(path, profile)
    name, volume = db_names(profile)
    if not owned('volume', volume, profile):
        if profile.get('database_initialized') or (path / 'client-login.json').exists():
            raise RuntimeError('persistent database volume is missing; restore a backup before restarting; no empty replacement created')
        docker_operation('volume', 'create', '--label', f'dev.lumen.local-profile={label(profile)}', volume)
    previous = owned('container', name, profile)
    if previous and previous['State']['Running']:
        bindings = previous['HostConfig'].get('PortBindings', {}).get('5432/tcp')
        expected = [{'HostIp': '127.0.0.1', 'HostPort': str(profile['ports']['postgres'])}]
        mounts = previous.get('Mounts', [])
        if bindings != expected or not any(item.get('Name') == volume and item.get('Destination') == '/var/lib/postgresql' for item in mounts):
            raise RuntimeError('owned database configuration differs from its profile; left running')
        if not profile.get('database_initialized'):
            profile['database_initialized'] = True
            write_json(path / 'profile.json', profile)
        return
    check_ports({'postgres': profile['ports']['postgres']})
    if previous:
        docker_operation('rm', name)
    env = path / 'postgres.env'
    env.write_text(f'POSTGRES_USER=whio\nPOSTGRES_DB=whio\nPOSTGRES_PASSWORD={profile["postgres_password"]}\n')
    env.chmod(0o600)
    docker_operation('run', '--detach', '--name', name, '--pull=never',
                     '--label', f'dev.lumen.local-profile={label(profile)}',
                     '--memory', '512m', '--memory-swap', '512m', '--cpus', '1',
                     '--log-driver', 'local', '--log-opt', 'max-size=8m', '--log-opt', 'max-file=2',
                     '--publish', f'127.0.0.1:{profile["ports"]["postgres"]}:5432',
                     '--volume', f'{volume}:/var/lib/postgresql', '--env-file', str(env),
                     'postgres:18-alpine', '-c', 'shared_buffers=32MB', '-c', 'max_connections=40',
                     '-c', 'max_wal_size=128MB', '-c', 'min_wal_size=32MB')
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        result = subprocess.run(['docker', 'exec', name, 'pg_isready', '-h', '127.0.0.1',
                                 '-U', 'whio', '-d', 'whio'], capture_output=True, timeout=5)
        if result.returncode == 0:
            profile['database_initialized'] = True
            write_json(path / 'profile.json', profile)
            return
        time.sleep(0.2)
    raise RuntimeError('profile Postgres did not become ready within 30 seconds')


def environment(path, profile, full):
    ports = profile['ports']
    result = {'WHIO_DATABASE_URL': f'postgres://whio:{profile["postgres_password"]}@127.0.0.1:{ports["postgres"]}/whio',
              'WHIO_CREDENTIAL_KEY': profile['credential_key'], 'WHIO_BIND_ADDRESS': f'127.0.0.1:{ports["api"]}',
              'WHIO_MEDIA_ROOTS': str(state_root() / 'music'), 'WHIO_LOG_LEVEL': 'info',
              'WHIO_YOUTUBE_ENABLED': str(full).lower(), 'WHIO_ARTWORK_CACHE_DIR': str(path / 'artwork'),
              'WHIO_ARTWORK_CACHE_BYTES': str(256 * 1024 ** 2), 'WHIO_SPOOL_DIR': str(path / 'spool'),
              'WHIO_SPOOL_MAX_BYTES': str(256 * 1024 ** 2), 'WHIO_SPOOL_BUDGET_BYTES': str(512 * 1024 ** 2),
              'WHIO_PLAYBACK_MAX_SESSIONS': '4', 'WHIO_WORKER_BIND_ADDRESS': f'127.0.0.1:{ports["worker"]}',
              'WHIO_WORKER_QUEUES': 'whio.jobs.probe,whio.metadata.refresh,whio.listening.delivery',
              'WHIO_WORKER_MAX_WORKERS': '1', 'WHIO_WORKER_POLL_INTERVAL_MILLISECONDS': '1000',
              'WHIO_WORKER_DRAIN_TIMEOUT_SECONDS': '30', 'DENO_DIR': str(path / 'deno')}
    if full:
        result.update({'WHIO_YOUTUBE_RESOLVER_URL': f'http://127.0.0.1:{ports["youtube"]}',
                       'WHIO_METADATA_RESOLVER_URL': f'http://127.0.0.1:{ports["metadata"]}'})
    return result


def clean_environment():
    allowed = ('PATH', 'HOME', 'USER', 'LOGNAME', 'LANG', 'LC_ALL', 'LC_CTYPE', 'TMPDIR',
               'SSL_CERT_FILE', 'SSL_CERT_DIR')
    return {key: value for key, value in os.environ.items() if key in allowed}


def socket_path(profile):
    root = checks.runtime_root()
    return root / f'local-{profile["name"]}.sock'


def supervisor(path, profile, *arguments):
    log = 'supervisor.log' if arguments[0] == 'up' else 'supervisor-client.log'
    command = ['process-compose', *arguments, '--use-uds', '--unix-socket', str(socket_path(profile)),
               '--log-file', str(path / 'logs' / log)]
    result = subprocess.run(command, cwd=path, env=clean_environment(), capture_output=True, text=True, timeout=55)
    if result.returncode:
        raise RuntimeError('profile supervisor command failed; inspect its logs')
    return result.stdout


def ready(port):
    try:
        with urllib.request.urlopen(f'http://127.0.0.1:{port}/health/ready', timeout=1) as response:
            return response.status == 200
    except OSError:
        return False


def process_alive(pid):
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False


def stop(path, profile, database=True):
    if profile.get('runtime') == 'docker':
        import dev_docker
        return dev_docker.stop(path, profile, database)
    if socket_path(profile).exists():
        states = json.loads(supervisor(path, profile, 'process', 'list', '--output', 'json'))
        pids = [item['pid'] for item in states if item.get('is_running') and item.get('pid', 0) > 0]
        supervisor(path, profile, 'down')
        deadline = time.monotonic() + 50
        while (socket_path(profile).exists() or any(process_alive(pid) for pid in pids)) and time.monotonic() < deadline:
            time.sleep(0.2)
        if socket_path(profile).exists() or any(process_alive(pid) for pid in pids):
            raise RuntimeError('owned server processes did not finish shutdown; database left running')
    name, _ = db_names(profile)
    container = owned('container', name, profile)
    if database and container and container['State']['Running']:
        docker_operation('stop', '--time', '10', name)
    print(f'{profile["name"]} stopped; database and music retained', flush=True)


def runtime_links(path, profile, previous=None):
    root = builds_root(profile).resolve()
    for name, target in (('current-rust', root / profile.get('rust_build', profile['build'])),
                         ('current-resolver', root / profile['build'] / 'resolver')):
        link = path / name
        if link.exists() or link.is_symlink():
            old_root = builds_root(previous).resolve() if previous else root
            old_target = old_root / (previous.get('rust_build', previous['build']) if name == 'current-rust' else previous['build']) if previous else None
            if previous and name == 'current-resolver':
                old_target /= 'resolver'
            if not link.is_symlink() or not (link.resolve().is_relative_to(root) or old_target and link.resolve() == old_target):
                raise RuntimeError('runtime link has another owner; left untouched')
        staging = path / (name + '-' + uuid.uuid4().hex)
        staging.symlink_to(target, target_is_directory=True)
        staging.replace(link)


def config(path, profile, full):
    if profile.get('runtime') == 'docker':
        import dev_docker
        return dev_docker.config(path, profile, full)
    installed = state_root() / 'builds' / profile['build']
    rust = state_root() / 'builds' / profile.get('rust_build', profile['build'])
    python = installed / 'resolver'
    if profile['name'] != 'personal':
        runtime_links(path, profile)
        rust = path / 'current-rust'
        python = path / 'current-resolver'
    if full:
        venv = state_root() / 'resolver-venv' / hashlib.sha256((python / 'uv.lock').read_bytes()).hexdigest()[:16]
        env = clean_environment()
        env['UV_PROJECT_ENVIRONMENT'] = str(venv)
        subprocess.run(['uv', 'sync', '--frozen', '--no-dev', '--python', '3.14'], cwd=python, env=env, check=True, timeout=180)
    else:
        venv = None
    common = environment(path, profile, full)
    env_file = path / 'server.env'
    env_file.write_text(''.join(f'{key}={value}\n' for key, value in common.items()))
    env_file.chmod(0o600)
    rotation = {'rotation': {'max_size_mb': 8, 'max_age_days': 3, 'max_backups': 2, 'compress': True}}
    processes = {}
    def process(name, command, port, cwd):
        return {'command': shlex.join(command), 'working_dir': str(cwd),
                'environment': [f'{key}={value}' for key, value in common.items()],
                'log_location': str(path / 'logs' / f'{name}.log'), 'log_configuration': rotation,
                'shutdown': {'signal': 15, 'timeout_seconds': 45},
                'readiness_probe': {'http_get': {'host': '127.0.0.1', 'port': port, 'path': '/health/ready'},
                                    'period_seconds': 1, 'timeout_seconds': 2, 'failure_threshold': 30}}
    if full:
        provider_values = {}
        provider_file = path / 'providers.env'
        if not provider_file.exists():
            provider_file.touch(mode=0o600)
        if provider_file.is_symlink() or provider_file.stat().st_mode & 0o077:
            raise RuntimeError('providers.env must be a regular mode-0600 file')
        for line in provider_file.read_text().splitlines():
            if not line.strip() or line.lstrip().startswith('#'):
                continue
            key, separator, value = line.partition('=')
            key = key.strip()
            if not separator or not re.fullmatch(r'WHIO_RESOLVER_[A-Z0-9_]+', key):
                raise RuntimeError('providers.env accepts only WHIO_RESOLVER_ settings')
            value = value.strip()
            if value.startswith(('"', "'")):
                words = shlex.split(value)
                if len(words) != 1:
                    raise RuntimeError('provider value must be a single string')
                value = words[0]
            provider_values[key] = value
        for name, module, port in (('youtube', 'app.main:app', profile['ports']['youtube']),
                                   ('metadata', 'app.metadata_main:app', profile['ports']['metadata'])):
            processes[name] = process(name, [str(venv / 'bin/python'), '-m', 'uvicorn', module,
                                             '--host', '127.0.0.1', '--port', str(port)], port, python)
            processes[name]['environment'] = [f'DENO_DIR={common["DENO_DIR"]}',
                                              *(f'{key}={value}' for key, value in provider_values.items())]
        processes['worker'] = process('worker', [str(rust / 'whio-worker')], profile['ports']['worker'], path)
        processes['worker']['depends_on'] = {name: {'condition': 'process_healthy'} for name in ('youtube', 'metadata')}
    processes['api'] = process('api', [str(rust / 'whio-api')], profile['ports']['api'], path)
    if full:
        processes['api']['depends_on'] = {name: {'condition': 'process_healthy'} for name in ('youtube', 'metadata')}
    write_json(path / 'process-compose.json', {'version': '0.5', 'log_level': 'info', 'processes': processes})


def start(path, profile, full, preserve_database=False):
    if profile.get('runtime') == 'docker':
        import dev_docker
        return dev_docker.start(path, profile, full)
    if not profile['build']:
        raise RuntimeError('profile has no installed build; run just setup first')
    if socket_path(profile).exists():
        if profile['full'] != full:
            raise RuntimeError('profile is already running in another mode; stop it first')
        if ready(profile['ports']['api']):
            print(f'{profile["name"]} already running at http://127.0.0.1:{profile["ports"]["api"]}')
            return
        raise RuntimeError('supervisor exists but API is not ready; inspect status/logs before restarting')
    checks.check_storage(checks.runtime_root())
    if profile['name'].startswith('preview-'):
        running = docker_operation('ps', '--filter', 'label=dev.lumen.local-profile', '--format', '{{.Names}}')
        prefix = f'lumen-local-{os.getuid()}-preview-'
        own_name, _ = db_names(profile)
        if any(name.startswith(prefix) and name != own_name for name in running.splitlines()):
            raise RuntimeError('one preview is already active; stop it before starting another')
    dev_media.mount(state_root())
    names = ('api', 'worker', 'youtube', 'metadata') if full else ('api',)
    check_ports({name: profile['ports'][name] for name in names})
    config(path, profile, full)
    start_db(path, profile)
    try:
        supervisor(path, profile, 'up', '--config', str(path / 'process-compose.json'), '--detached',
                   '--tui=false', '--disable-dotenv', '--ordered-shutdown',
                   '--log-file', str(path / 'logs' / 'supervisor.log'))
        deadline = time.monotonic() + 45
        while time.monotonic() < deadline:
            if all(ready(profile['ports'][name]) for name in names):
                profile['full'] = full
                if profile['name'].startswith('preview-'):
                    profile['expires'] = time.time() + 86400
                write_json(path / 'profile.json', profile)
                print(f'{profile["name"]} ready at http://127.0.0.1:{profile["ports"]["api"]}; mode={"full" if full else "local"}', flush=True)
                return
            time.sleep(0.2)
        raise RuntimeError('profile did not become ready within 45 seconds; inspect logs')
    except BaseException:
        stop(path, profile, database=not preserve_database)
        raise


def create_user(path, profile):
    if profile.get('runtime') == 'docker':
        import dev_docker
        return dev_docker.create_user(path, profile)
    credentials = path / 'client-login.json'
    if credentials.exists():
        print(f'client login: {credentials}')
        return
    installed = state_root() / 'builds' / profile['build']
    env = clean_environment()
    env.update(environment(path, profile, profile['full']))
    cli = str(installed / 'whio-cli')
    name, _ = db_names(profile)
    exists = docker_operation('exec', name, 'psql', '-U', 'whio', '-d', 'whio', '-tAc',
                              "select exists(select 1 from users where username='local')")
    actions = [] if exists == 't' else [['create-user', 'local', 'local']]
    actions.append(['assign-admin', 'local'])
    for arguments in actions:
        result = subprocess.run([cli, *arguments], cwd=path, env=env, capture_output=True, timeout=15)
        if result.returncode:
            raise RuntimeError('local user creation failed; existing users were retained')
    result = subprocess.run([cli, 'mint-app-password', 'local', 'laptop'], cwd=path, env=env,
                            capture_output=True, text=True, timeout=15)
    if result.returncode:
        raise RuntimeError('local app-password creation failed')
    password = next((line.removeprefix('password: ') for line in result.stdout.splitlines() if line.startswith('password: ')), None)
    if not password:
        raise RuntimeError('CLI did not return an app password')
    write_json(credentials, {'server': f'http://127.0.0.1:{profile["ports"]["api"]}',
                            'username': 'local', 'password': password})
    print(f'client login: {credentials}', flush=True)


def backup(path, profile):
    start_db(path, profile)
    name, _ = db_names(profile)
    directory = path / 'backups' / (time.strftime('%Y%m%d-%H%M%S') + '-' + uuid.uuid4().hex[:6])
    directory.mkdir(mode=0o700)
    dump = directory / 'database.dump'
    with dump.open('wb') as stream:
        result = subprocess.run([*docker_args(profile), 'exec', name, 'pg_dump', '-U', 'whio', '-d', 'whio', '-Fc'],
                                stdout=stream, stderr=subprocess.PIPE, timeout=120)
    dump.chmod(0o600)
    if result.returncode or dump.stat().st_size == 0:
        raise RuntimeError('backup failed; previous backups retained')
    for name in ('profile.json', 'client-login.json'):
        if (path / name).exists():
            shutil.copy2(path / name, directory / name)
    write_json(directory / 'backup.json', {'created': time.time(), 'profile': profile['name'],
               'dump_sha256': dev_media.digest(dump), 'build': profile['build'],
               'rust_build': profile.get('rust_build')})
    complete = sorted((item for item in (path / 'backups').iterdir() if (item / 'backup.json').is_file()), reverse=True)
    for old in complete[2:]:
        shutil.rmtree(old)
    print(f'backup saved: {directory}', flush=True)
    return directory


def backup_check(path, profile):
    if profile.get('runtime') == 'docker':
        import dev_docker
        return dev_docker.backup_check(path, profile)
    backups = sorted(item for item in (path / 'backups').iterdir() if (item / 'backup.json').is_file())
    if not backups:
        raise RuntimeError('no completed backup exists; run just backup first')
    directory = backups[-1]
    document = json.loads((directory / 'backup.json').read_text())
    dump = directory / 'database.dump'
    if dev_media.digest(dump) != document['dump_sha256']:
        raise RuntimeError('backup hash differs; existing databases left untouched')
    saved = json.loads((directory / 'profile.json').read_text())
    name = f'lumen-test-{uuid.uuid4().hex[:12]}'
    process = None
    root = checks.runtime_root()
    with checks.admission(root, checks.cache_key(REPO), 60) as (slot, descriptors):
        try:
            database = checks.start_postgres(name, slot)
            with dump.open('rb') as stream:
                result = subprocess.run(['docker', 'exec', '-i', name, 'pg_restore', '--exit-on-error',
                                         '--no-owner', '-U', 'whio', '-d', 'whio'], stdin=stream,
                                        capture_output=True, timeout=60)
            if result.returncode:
                raise RuntimeError('backup restore failed in disposable database')
            port = free_port()
            env = clean_environment()
            env.update(environment(path, saved, False))
            env.update({'WHIO_DATABASE_URL': database, 'WHIO_BIND_ADDRESS': f'127.0.0.1:{port}'})
            with tempfile.TemporaryDirectory(prefix='lumen-backup-check-') as temporary:
                env['WHIO_ARTWORK_CACHE_DIR'] = str(Path(temporary) / 'artwork')
                env['WHIO_SPOOL_DIR'] = str(Path(temporary) / 'spool')
                with (Path(temporary) / 'api.log').open('wb') as log:
                    binary = state_root() / 'builds' / document['build'] / 'whio-api'
                    process = subprocess.Popen([str(binary)], cwd=temporary, env=env, stdout=log,
                                               stderr=subprocess.STDOUT, start_new_session=True, pass_fds=descriptors)
                    deadline = time.monotonic() + 30
                    while not ready(port) and time.monotonic() < deadline and process.poll() is None:
                        time.sleep(0.2)
                    if not ready(port):
                        raise RuntimeError('restored app did not become ready')
                    login = json.loads((directory / 'client-login.json').read_text())
                    salt = secrets.token_hex(8)
                    query = urllib.parse.urlencode({'u': login['username'], 's': salt,
                            't': hashlib.md5((login['password'] + salt).encode()).hexdigest(),
                            'v': '1.16.1', 'c': 'backup-check', 'f': 'json'})
                    with urllib.request.urlopen(f'http://127.0.0.1:{port}/rest/ping?{query}', timeout=5) as response:
                        if json.load(response)['subsonic-response']['status'] != 'ok':
                            raise RuntimeError('saved client credential cannot authenticate after restore')
                    counts = checks.docker('exec', name, 'psql', '-U', 'whio', '-d', 'whio', '-tAc',
                             "select json_build_object('users',(select count(*) from users),'tracks',(select count(*) from tracks),'playlists',(select count(*) from playlists),'listens',(select count(*) from listens))")
                    print(f'backup restored to disposable Postgres; saved key/login authenticated; {counts}', flush=True)
                    checks.stop_process_group(process)
                    process = None
        finally:
            if process:
                checks.stop_process_group(process)
            checks.cleanup_postgres(name)


def status(path, profile):
    if profile.get('runtime') == 'docker':
        import dev_docker
        return dev_docker.status(path, profile)
    name, volume = db_names(profile)
    container = owned('container', name, profile)
    print(f'{profile["name"]}: API {"ready" if ready(profile["ports"]["api"]) else "stopped"}; '
          f'Postgres {"running" if container and container["State"]["Running"] else "stopped"}; '
          f'build={(profile["build"] or "none")[:12]}; mode={"full" if profile["full"] else "local"}')
    print(f'url: http://127.0.0.1:{profile["ports"]["api"]}\nstate: {path}\nvolume: {volume}')
    if (path / 'client-login.json').exists():
        print(f'client login: {path / "client-login.json"}')
    if container and container['State']['Running']:
        size = docker_operation('exec', name, 'du', '-sk', '/var/lib/postgresql').split()[0]
        print(f'postgres allocated: {int(size) / 1024:.1f} MiB; memory cap: 512 MiB')
    result = subprocess.run(['du', '-skx', str(state_root())], capture_output=True, text=True, check=True)
    print(f'host state allocated: {int(result.stdout.split()[0]) / 1024:.1f} MiB; '
          f'free disk: {shutil.disk_usage(STATE).free / 1024 ** 3:.1f} GiB')


def observe(path, profile, action, service):
    if profile.get('runtime') == 'docker':
        import dev_docker
        return dev_docker.observe(path, profile, action, service)
    if action == 'services':
        if socket_path(profile).exists():
            print(supervisor(path, profile, 'process', 'list', '--output', 'wide'), end='')
        else:
            print('app supervisor stopped')
        name, _ = db_names(profile)
        container = owned('container', name, profile)
        print(f'postgres: {"running" if container and container["State"]["Running"] else "stopped"}')
        return 0
    if action == 'logs' and service == 'postgres':
        name, _ = db_names(profile)
        if not owned('container', name, profile):
            raise RuntimeError('profile Postgres container does not exist')
        command = ['docker', 'logs', '--tail', '200', '--follow', name]
    else:
        if not socket_path(profile).exists():
            raise RuntimeError('app supervisor stopped; start the profile first or inspect its saved log files')
        command = ['process-compose', 'attach', '--read-only'] if action == 'monitor' else [
            'process-compose', 'process', 'logs', service, '--tail', '200', '--follow']
        command.extend(('--use-uds', '--unix-socket', str(socket_path(profile)),
                        '--log-file', str(path / 'logs' / 'viewer-client.log')))
    environment = clean_environment()
    if action == 'monitor':
        if not sys.stdin.isatty() or not sys.stdout.isatty():
            raise RuntimeError('monitor needs an interactive terminal; use just services for a text listing')
        terminal = os.environ.get('TERM', '')
        environment['TERM'] = terminal if terminal and terminal != 'dumb' else 'xterm-256color'
        if os.environ.get('COLORTERM'):
            environment['COLORTERM'] = os.environ['COLORTERM']
    return subprocess.run(command, cwd=path, env=environment).returncode


def cleanup():
    root = state_root()
    import dev_docker
    dev_docker.cleanup()
    for file in (root / 'profiles').glob('preview-*/profile.json'):
        path, profile = load_profile(file.parent.name)
        if profile.get('expires') is None or profile['expires'] > time.time():
            continue
        if running(path, profile):
            print(f'retaining running preview {profile["name"]}')
            continue
        name, volume = db_names(profile)
        container = owned('container', name, profile)
        if container and container['State']['Running']:
            print(f'retaining running database {name}')
            continue
        if container:
            docker_operation('rm', name, profile=profile)
        if owned('volume', volume, profile):
            docker_operation('volume', 'rm', volume, profile=profile)
        shutil.rmtree(path)
        print(f'removed expired stopped preview {profile["name"]}')
    prune_installed_builds(root)
    checks.prune_caches(checks.runtime_root())
    checks.check_storage(checks.runtime_root())
    print('managed cache retention checked; warm caches within the budget retained')


def main():
    parser = argparse.ArgumentParser(description='bounded laptop development stack')
    parser.add_argument('command', choices=('setup', 'start', 'stop', 'status', 'reload', 'upgrade', 'backup', 'backup-check', 'login', 'cleanup', 'media-import', 'media-verify', 'watch', 'watch-step', 'migrate', 'migrations', 'monitor', 'services', 'logs', 'containerize', 'resources'))
    parser.add_argument('profile', nargs='?', default='personal', type=profile_name)
    parser.add_argument('--service', choices=('api', 'worker', 'youtube', 'metadata', 'postgres'), default='api')
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument('--full', action='store_true')
    mode.add_argument('--local', action='store_true')
    parser.add_argument('--watch-token')
    parser.add_argument('--runtime', choices=('docker', 'native'))
    parser.add_argument('--remote')
    parser.add_argument('--source')
    parser.add_argument('--manifest', type=Path)
    args = parser.parse_args()
    if (args.full or args.local) and args.command != 'start':
        parser.error('--full/--local apply only to start')
    if (args.remote or args.source or args.manifest) and args.command != 'media-import':
        parser.error('--remote/--source/--manifest apply only to media-import')
    if args.command == 'media-import' and args.manifest is None:
        parser.error('media-import requires --manifest with the selected album directories')
    if args.runtime and args.command != 'setup':
        parser.error('--runtime applies only to setup')
    signal.signal(signal.SIGTERM, checks.interrupt)
    try:
        if args.command in ('monitor', 'services', 'logs'):
            path, profile = load_profile(args.profile)
            return observe(path, profile, args.command, args.service)
        if args.command == 'watch':
            import dev_watch
            return dev_watch.watch(args.profile)
        with state_lock():
            if args.command == 'media-import':
                dev_media.import_media(state_root(), args.remote, args.source, args.manifest)
            elif args.command == 'media-verify':
                dev_media.verify(state_root())
            elif args.command == 'cleanup':
                cleanup()
            else:
                path, profile = load_profile(args.profile, create=args.command == 'setup')
                if args.command == 'setup':
                    cleanup()
                    if args.runtime:
                        if profile['build'] and profile.get('runtime', 'native') != args.runtime:
                            raise RuntimeError('existing runtime changes require just containerize')
                        profile['runtime'] = args.runtime
                    if profile.get('runtime') == 'docker':
                        import dev_docker
                        dev_docker.configure_profile(path, profile)
                    if profile['build'] and profile['build'] != current_identity(profile):
                        action = 'upgrade' if args.profile == 'personal' else 'reload'
                        raise RuntimeError(f'profile already has an installed build; use just {action} {args.profile} to change source')
                    build(path, profile)
                elif args.command == 'containerize':
                    import dev_docker
                    dev_docker.convert(path, profile)
                elif args.command == 'resources':
                    import dev_docker
                    dev_docker.resources(profile)
                elif args.command == 'start':
                    cleanup()
                    start(path, profile, False if args.local else args.full or profile['full'])
                    create_user(path, profile)
                elif args.command == 'stop':
                    stop(path, profile)
                elif args.command == 'status':
                    status(path, profile)
                elif args.command == 'login':
                    create_user(path, profile)
                elif args.command == 'backup':
                    backup(path, profile)
                elif args.command == 'backup-check':
                    backup_check(path, profile)
                elif args.command in ('watch-step', 'migrate', 'migrations'):
                    import dev_watch
                    if args.command == 'watch-step':
                        dev_watch.watch_step(path, profile, args.watch_token)
                    elif args.command == 'migrations':
                        dev_watch.migration_status(path, profile)
                    else:
                        dev_watch.reload(path, profile, migrations=True)
                elif args.command in ('reload', 'upgrade'):
                    if args.profile == 'personal' and args.command != 'upgrade':
                        raise RuntimeError('personal source changes require just upgrade personal, which backs up first')
                    if args.command == 'reload':
                        import dev_watch
                        dev_watch.reload(path, profile, migrations=True)
                    elif profile.get('runtime') == 'docker':
                        import dev_docker
                        dev_docker.upgrade(path, profile)
                    else:
                        backup(path, profile)
                        old = dict(profile)
                        profile.pop('rust_build', None)
                        build(path, profile)
                        stop(path, old)
                        start(path, profile, old['full'])
                        create_user(path, profile)
        return 0
    except KeyboardInterrupt:
        return 130
    except checks.Interrupted as error:
        return 128 + error.signum
    except (OSError, ValueError, KeyError, RuntimeError, subprocess.SubprocessError) as error:
        print(f'dev setup failed: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
