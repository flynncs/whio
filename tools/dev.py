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
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
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
        if value.get('runtime') == 'native':
            raise RuntimeError('native profiles are no longer supported; recreate it')
        return path, value
    if not create:
        raise RuntimeError(f'{name} is not set up; run just up {name}')
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


def source_root(root=None):
    return REPO if root is None else root


def source_files(root=None):
    root = source_root(root)
    files = []
    for directory in ('core', 'resolver-client-generated', 'subsonic-types-generated', 'ytm-resolver'):
        base = root / 'apps' / directory
        for parent, directories, names in os.walk(base):
            directories[:] = [name for name in directories if name not in
                              ('target', '.venv', '__pycache__', '.pytest_cache', '.ruff_cache')]
            for name in names:
                path = Path(parent) / name
                if path.suffix in ('.rs', '.sql', '.py', '.toml', '.lock') or name == 'README.md' or path.is_relative_to(base / 'tests/fixtures'):
                    if path.is_symlink():
                        raise RuntimeError('build source must not be a symlink')
                    files.append(path)
    for path in (root / 'rust-toolchain.toml', root / '.cargo/config.toml', root / '.cargo/config'):
        if path.exists():
            if path.is_symlink():
                raise RuntimeError('build source must not be a symlink')
            files.append(path)
    return sorted(files)


def source_identity(root=None):
    root = source_root(root)
    digest = hashlib.sha256()
    for path in source_files(root):
        digest.update(str(path.relative_to(root)).encode() + b'\0')
        digest.update(path.read_bytes())
        digest.update(b'\0')
    return digest.hexdigest()


def builds_root(profile):
    directory = state_root() / 'container-builds' / profile['architecture']
    if directory.is_symlink() or directory.parent.is_symlink():
        raise RuntimeError('container build directory must not be a symlink')
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    return directory


def current_identity(profile):
    import dev_docker
    return dev_docker.identity(profile)


def running(path, profile):
    import dev_docker
    return dev_docker.running(path, profile)


def docker_args(profile=None):
    context = (profile or {}).get('docker_context')
    return ['docker', '--context', context] if context else ['docker']


def prune_installed_builds(root):
    referenced = set()
    for file in (root / 'profiles').glob('*/profile.json'):
        value = json.loads(file.read_text())
        if value.get('runtime') != 'docker' or not value.get('architecture') or not value.get('build'):
            continue
        base = builds_root(value)
        referenced.update(base / identity for identity in (value['build'], value.get('rust_build')) if identity)
    for file in (root / 'profiles').glob('*/backups/*/backup.json'):
        value = json.loads(file.read_text())
        saved_file = file.parent / 'profile.json'
        saved = json.loads(saved_file.read_text()) if saved_file.exists() else {}
        if saved.get('runtime') != 'docker' or not saved.get('architecture'):
            continue
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


def build(path, profile, persist=True, root=None, commit=None):
    import dev_docker
    return dev_docker.build(path, profile, persist, root=root, commit=commit)


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
    import dev_docker
    return dev_docker.start_db(path, profile)


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


def ready(port):
    try:
        with urllib.request.urlopen(f'http://127.0.0.1:{port}/health/ready', timeout=1) as response:
            return response.status == 200
    except OSError:
        return False


def stop(path, profile, database=True):
    import dev_docker
    return dev_docker.stop(path, profile, database)


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
    import dev_docker
    return dev_docker.config(path, profile, full)


def start(path, profile, full):
    import dev_docker
    return dev_docker.start(path, profile, full)


def create_user(path, profile):
    import dev_docker
    return dev_docker.create_user(path, profile)


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
    import dev_docker
    return dev_docker.backup_check(path, profile)


def status(path, profile):
    import dev_docker
    return dev_docker.status(path, profile)


def overview():
    root = state_root()
    for file in sorted((root / 'profiles').glob('*/profile.json')):
        profile = json.loads(file.read_text())
        state = 'ready' if ready(profile['ports']['api']) else (
            'running' if running(file.parent, profile) else 'stopped')
        print(f'{profile["name"]}  http://127.0.0.1:{profile["ports"]["api"]}  '
              f'{(profile["build"] or "none")[:12]}  {state}')
    return 0


def up(path, profile):
    cleanup()
    if not profile['build']:
        import dev_docker
        dev_docker.configure_profile(path, profile)
        build(path, profile)
    start(path, profile, True)
    create_user(path, profile)


@contextmanager
def main_snapshot():
    revision = subprocess.run(['git', '-C', str(REPO), 'rev-parse', '--verify', 'refs/heads/main^{commit}'],
                              capture_output=True, text=True)
    if revision.returncode:
        raise RuntimeError('no main branch')
    root = Path(tempfile.mkdtemp(prefix='lumen-main-'))
    try:
        archive = subprocess.Popen(['git', '-C', str(REPO), 'archive', 'refs/heads/main'],
                                   stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        extract = subprocess.run(['tar', '-x', '-C', str(root)], stdin=archive.stdout,
                                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        archive.stdout.close()
        if archive.wait() or extract.returncode:
            raise RuntimeError('main export failed')
        yield root, revision.stdout.strip()
    finally:
        shutil.rmtree(root, ignore_errors=True)


def deploy(path, profile):
    if profile['name'] != 'personal':
        raise RuntimeError('deploy updates personal only')
    with main_snapshot() as (root, commit):
        print(f'deploying main {commit[:12]}', flush=True)
        import dev_docker
        return dev_docker.upgrade(path, profile, root=root, commit=commit)


def logs(path, profile, service):
    import dev_docker
    return dev_docker.observe(path, profile, 'logs', service)


def cleanup():
    root = state_root()
    import dev_docker
    dev_docker.cleanup()
    for file in (root / 'profiles').glob('preview-*/profile.json'):
        path, profile = load_profile(file.parent.name)
        if profile.get('expires') is None or profile['expires'] > time.time():
            continue
        if running(path, profile):
            continue
        name, volume = db_names(profile)
        container = owned('container', name, profile)
        if container and container['State']['Running']:
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


def clean():
    cleanup()
    print('clean complete; expired previews and idle caches reclaimed', flush=True)


COMMANDS = ('status', 'up', 'down', 'logs', 'deploy', 'dev', 'migrations', 'backup',
            'clean', 'media-import', 'media-verify')
INTERNAL_COMMANDS = ('watch-step',)


def main():
    parser = argparse.ArgumentParser(description='bounded laptop development stack')
    parser.add_argument('command', metavar='{' + ','.join(COMMANDS) + '}')
    parser.add_argument('profile', nargs='?', type=profile_name)
    parser.add_argument('--service', choices=('api', 'worker', 'youtube', 'metadata', 'postgres'))
    parser.add_argument('--watch-token')
    parser.add_argument('--remote')
    parser.add_argument('--source')
    parser.add_argument('--manifest', type=Path)
    args = parser.parse_args()
    if args.command not in COMMANDS + INTERNAL_COMMANDS:
        parser.error(f'unknown command {args.command!r}; choose from {", ".join(COMMANDS)}')
    if args.service is not None and args.command != 'logs':
        parser.error('--service applies only to logs')
    if args.watch_token is not None and args.command != 'watch-step':
        parser.error('--watch-token applies only to watch-step')
    if (args.remote or args.source or args.manifest) and args.command != 'media-import':
        parser.error('--remote/--source/--manifest apply only to media-import')
    if args.command == 'media-import' and args.manifest is None:
        parser.error('media-import requires --manifest with the selected album directories')
    if args.profile is None and args.command != 'status':
        args.profile = 'personal'
    signal.signal(signal.SIGTERM, checks.interrupt)
    try:
        if args.command == 'status' and args.profile is None:
            return overview()
        if args.command == 'dev':
            import dev_watch
            return dev_watch.watch(args.profile)
        if args.command == 'logs':
            path, profile = load_profile(args.profile)
            return logs(path, profile, args.service)
        with state_lock():
            if args.command == 'media-import':
                dev_media.import_media(state_root(), args.remote, args.source, args.manifest)
            elif args.command == 'media-verify':
                dev_media.verify(state_root())
            elif args.command == 'clean':
                clean()
            else:
                path, profile = load_profile(args.profile, create=args.command == 'up')
                if args.command == 'up':
                    up(path, profile)
                elif args.command == 'down':
                    stop(path, profile)
                elif args.command == 'status':
                    status(path, profile)
                elif args.command == 'deploy':
                    deploy(path, profile)
                elif args.command == 'backup':
                    backup(path, profile)
                    backup_check(path, profile)
                elif args.command == 'migrations':
                    import dev_watch
                    dev_watch.migration_status(path, profile)
                elif args.command == 'watch-step':
                    import dev_watch
                    dev_watch.watch_step(path, profile, args.watch_token)
        return 0
    except KeyboardInterrupt:
        return 130
    except checks.Interrupted as error:
        return 128 + error.signum
    except (OSError, ValueError, KeyError, RuntimeError, subprocess.SubprocessError) as error:
        print(f'{args.command} failed: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
