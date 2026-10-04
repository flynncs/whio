import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import signal
import subprocess
import sys
import time
import urllib.parse
import urllib.request
import uuid

import dev
import dev_checks as checks


OWNER = 'dev.lumen.cache-owner'
KIND = 'dev.lumen.cache-kind'


def command(profile, *arguments, timeout=30):
    result = subprocess.run([*dev.docker_args(profile), *arguments], capture_output=True,
                            text=True, timeout=timeout, env=dev.clean_environment())
    if result.returncode:
        diagnostic = result.stderr
        for key in ('postgres_password', 'credential_key'):
            if profile.get(key):
                diagnostic = diagnostic.replace(profile[key], '<profile secret>')
        file = dev.state_root() / ('engine-' + hashlib.sha256(profile.get('docker_context', 'default').encode()).hexdigest()[:12] + '-error.log')
        file.write_text(diagnostic)
        file.chmod(0o600)
        raise RuntimeError(f'Docker {arguments[0]} failed; details: {file}')
    return result.stdout.strip()


def remove_container(profile, name):
    remaining = dev.inspect('container', name, profile)
    if not remaining:
        return
    if (remaining['Config'].get('Labels') or {}).get('dev.lumen.local-profile') != dev.label(profile):
        raise RuntimeError('throwaway container owner differs')
    command(profile, 'rm', '--force', name)


def configure_profile(path, profile, persist=True):
    if not profile.get('docker_context'):
        profile['docker_context'] = command({}, 'context', 'show')
    architecture = command(profile, 'info', '--format', '{{.Architecture}}')
    architecture = {'aarch64': 'arm64', 'x86_64': 'amd64'}.get(architecture, architecture)
    if architecture not in ('arm64', 'amd64'):
        raise RuntimeError('the local launcher supports Linux arm64/amd64 engines')
    if profile.get('architecture', architecture) != architecture:
        raise RuntimeError('profile architecture differs from its engine; rebuild into a separate profile')
    profile['architecture'] = architecture
    if not profile.get('database_container'):
        name, volume = dev.db_names(profile)
        if not profile.get('database_initialized'):
            name += '-' + hashlib.sha256(str(dev.STATE).encode()).hexdigest()[:8]
            volume = name + '-pg'
        profile.update(database_container=name, database_volume=volume)
    if not profile.get('media_root'):
        supplied = os.environ.get('LUMEN_MEDIA_ROOT')
        profile['media_root'] = str(Path(supplied).expanduser().resolve()) if supplied else str(dev.STATE / 'music')
    if persist:
        dev.write_json(path / 'profile.json', profile)


def image_names(profile, root=None):
    root = dev.source_root(root)
    values = {}
    for name, paths in {
        'compiler': ('docker/local.Dockerfile', 'rust-toolchain.toml'),
        'core': ('docker/local.Dockerfile',),
        'resolver': ('docker/ytm-resolver.Dockerfile', 'apps/ytm-resolver/pyproject.toml', 'apps/ytm-resolver/uv.lock'),
    }.items():
        digest = hashlib.sha256()
        for path in paths:
            digest.update(path.encode() + b'\0' + (root / path).read_bytes())
        values[name] = f'lumen-dev-{os.getuid()}/{name}:{digest.hexdigest()[:20]}-{profile["architecture"]}'
    return values


def identity(profile, root=None):
    root = dev.source_root(root)
    return hashlib.sha256((dev.source_identity(root) + json.dumps(image_names(profile, root), sort_keys=True)).encode()).hexdigest()


def project(profile):
    root = hashlib.sha256(str(dev.STATE).encode()).hexdigest()[:8]
    return f'lumen-{os.getuid()}-{root}-{profile["name"]}'


def context_key(profile):
    return hashlib.sha256(profile['docker_context'].encode()).hexdigest()[:12]


def main_checkout_key():
    result = subprocess.run(['git', 'rev-parse', '--path-format=absolute', '--git-common-dir'],
                            cwd=dev.REPO, capture_output=True, text=True, timeout=30)
    if result.returncode:
        return None
    return checks.cache_key(Path(result.stdout.strip()).parent)


def clear_target_cache(profile, target):
    name = project(profile) + '-clear-' + uuid.uuid4().hex[:8]
    arguments = [*dev.docker_args(profile), 'run', '--rm', '--name', name,
                 '--label', f'dev.lumen.local-profile={dev.label(profile)}',
                 '--mount', f'type=volume,source={target},target=/to',
                 '--entrypoint', 'sh', profile['images']['compiler'], '-c', 'find /to -mindepth 1 -delete']
    try:
        result = subprocess.run(arguments, capture_output=True, timeout=30,
                                env=dev.clean_environment())
        if result.returncode:
            raise RuntimeError('failed reflink clone could not be cleared')
    except (OSError, subprocess.TimeoutExpired) as error:
        raise RuntimeError('failed reflink clone could not be cleared') from error
    finally:
        remove_container(profile, name)


def clone_target_cache(profile, target, main_key):
    source = build_volume_name(profile, main_key)
    if not dev.inspect('volume', source, profile):
        return
    name = project(profile) + '-clone-' + uuid.uuid4().hex[:8]
    try:
        arguments = [*dev.docker_args(profile), 'run', '--rm', '--name', name,
                     '--label', f'dev.lumen.local-profile={dev.label(profile)}',
                     '--mount', f'type=volume,source={source},target=/from',
                     '--mount', f'type=volume,source={target},target=/to',
                     '--entrypoint', 'cp', profile['images']['compiler'],
                     '-a', '--reflink=always', '/from/.', '/to/']
        try:
            result = subprocess.run(arguments, capture_output=True, timeout=300)
        except (subprocess.TimeoutExpired, OSError):
            result = subprocess.CompletedProcess([], 1)
        if result.returncode:
            clear_target_cache(profile, target)
            print('target cache was not cloned; build starts from an empty target', flush=True)
    finally:
        remove_container(profile, name)


def build_volume_name(profile, suffix):
    return f'lumen-build-{os.getuid()}-{context_key(profile)}-{profile["architecture"]}-{suffix}'


def cache_volume(profile, kind, key=None):
    name = build_volume_name(profile, key if kind == 'target' else kind)
    value = dev.inspect('volume', name, profile)
    expected = {OWNER: str(os.getuid()), KIND: kind}
    if key:
        expected['dev.lumen.checkout'] = key
    if value:
        if any((value.get('Labels') or {}).get(label) != text for label, text in expected.items()):
            raise RuntimeError('build volume has another owner; left untouched')
    else:
        arguments = ['volume', 'create']
        for label, text in expected.items():
            arguments.extend(('--label', f'{label}={text}'))
        command(profile, *arguments, name)
        main_key = main_checkout_key() if kind == 'target' else None
        if main_key and key and key != main_key:
            clone_target_cache(profile, name, main_key)
    return name


def media(profile):
    path = Path(profile['media_root'])
    if path == dev.STATE / 'music' and sys.platform == 'darwin':
        dev.dev_media.mount(dev.state_root())
    if not path.is_dir():
        raise RuntimeError('music directory is missing; set LUMEN_MEDIA_ROOT during setup or mount the imported copy')
    return path


def bind(source, target=None, readonly=True):
    return {'type': 'bind', 'source': str(source), 'target': str(target or source), 'read_only': readonly}


def providers(path):
    file = path / 'providers.env'
    if not file.exists():
        file.touch(mode=0o600)
    if file.is_symlink() or not file.is_file() or file.stat().st_mode & 0o077:
        raise RuntimeError('providers.env must be a regular mode-0600 file')
    result = {}
    for line in file.read_text().splitlines():
        if not line.strip() or line.lstrip().startswith('#'):
            continue
        key, separator, value = line.partition('=')
        if not separator or not re.fullmatch(r'WHIO_RESOLVER_[A-Z0-9_]+', key.strip()):
            raise RuntimeError('providers.env accepts only WHIO_RESOLVER_ settings')
        import shlex
        words = shlex.split(value.strip()) if value.strip().startswith(('"', "'")) else [value.strip()]
        if len(words) != 1:
            raise RuntimeError('provider setting must contain one value')
        result[key.strip()] = words[0]
    return result


def config(path, profile, full, source=None):
    names = profile.get('images') or image_names(profile)
    label = {'dev.lumen.local-profile': dev.label(profile), 'dev.lumen.state-root': str(dev.STATE)}
    pg, volume = dev.db_names(profile)
    key = checks.cache_key(dev.REPO)
    target = cache_volume(profile, 'target', key) if source else profile.get('target_volume')
    registry = cache_volume(profile, 'registry') if source else profile.get('registry_volume')
    common = dev.environment(path, profile, full)
    common.update(WHIO_DATABASE_URL=f'postgres://whio:{profile["postgres_password"]}@postgres:5432/whio',
                  WHIO_BIND_ADDRESS='0.0.0.0:3000', WHIO_WORKER_BIND_ADDRESS='0.0.0.0:3001',
                  WHIO_MEDIA_ROOTS=profile['media_root'])
    if full:
        common.update(WHIO_YOUTUBE_RESOLVER_URL='http://youtube:8000', WHIO_METADATA_RESOLVER_URL='http://metadata:8000')
    runtime_mounts = [bind(dev.builds_root(profile)), bind(path), bind(Path(profile['media_root'])),
                      bind(path / 'artwork', readonly=False), bind(path / 'spool', readonly=False)]
    services = {'postgres': {'image': 'postgres:18-alpine', 'container_name': pg,
                    'environment': {'POSTGRES_USER': 'whio', 'POSTGRES_DB': 'whio', 'POSTGRES_PASSWORD': profile['postgres_password']},
                    'volumes': ['postgres:/var/lib/postgresql'],
                    'ports': [f'127.0.0.1:{profile["ports"]["postgres"]}:5432'],
                    'mem_limit': '512m', 'memswap_limit': '512m', 'cpus': 1,
                    'command': ['postgres', '-c', 'shared_buffers=32MB', '-c', 'max_connections=40', '-c', 'max_wal_size=128MB', '-c', 'min_wal_size=32MB'],
                    'healthcheck': {'test': ['CMD', 'pg_isready', '-h', '127.0.0.1', '-U', 'whio', '-d', 'whio'], 'interval': '5s', 'timeout': '3s', 'retries': 6}}}
    volumes = {'postgres': {'external': True, 'name': volume}}
    for name, binary, port in (('api', 'whio-api', 3000), ('worker', 'whio-worker', 3001)):
        services[name] = {'image': names['core'], 'command': [str(path / 'current-rust' / binary)],
                    'user': f'{os.getuid()}:{os.getgid()}', 'environment': common, 'volumes': runtime_mounts,
                    'ports': [f'127.0.0.1:{profile["ports"][name]}:{port}'], 'mem_limit': '512m', 'cpus': 2,
                    'profiles': [] if name == 'api' else ['full'],
                    'healthcheck': {'test': ['CMD', 'curl', '--fail', '--silent', f'http://127.0.0.1:{port}/health/ready'],
                                    'interval': '10s', 'start_interval': '1s', 'start_period': '45s', 'timeout': '3s', 'retries': 3}}
    for name, module in (('youtube', 'app.main:app'), ('metadata', 'app.metadata_main:app')):
        directory = path / 'deno'
        directory.mkdir(mode=0o700, exist_ok=True)
        services[name] = {'image': names['resolver'], 'entrypoint': ['/opt/venv/bin/uvicorn'],
                    'command': [module, '--host', '0.0.0.0', '--port', '8000'], 'working_dir': str(path / 'current-resolver'),
                    'user': f'{os.getuid()}:{os.getgid()}', 'environment': dict(providers(path), DENO_DIR=str(directory), PYTHONDONTWRITEBYTECODE='1'),
                    'volumes': [bind(dev.builds_root(profile)), bind(path), bind(directory, readonly=False)],
                    'ports': [f'127.0.0.1:{profile["ports"][name]}:8000'], 'profiles': ['full'], 'mem_limit': '256m', 'cpus': 1,
                    'healthcheck': {'test': ['CMD', '/opt/venv/bin/python', '-c', 'import urllib.request; urllib.request.urlopen("http://127.0.0.1:8000/health/ready", timeout=2)'],
                                    'interval': '10s', 'start_interval': '1s', 'start_period': '45s', 'timeout': '3s', 'retries': 3}}
    if source:
        volumes.update(target={'external': True, 'name': target}, registry={'external': True, 'name': registry})
        services['builder'] = {'image': names['compiler'], 'profiles': ['build'], 'working_dir': '/workspace',
                    'command': ['sh', '-c', 'trap "exit 0" TERM INT; sleep infinity & wait'],
                    'volumes': [bind(source, '/workspace'), 'target:/target', 'registry:/usr/local/cargo/registry'],
                    'environment': {'CARGO_TARGET_DIR': '/target', 'CARGO_BUILD_JOBS': '2', 'CARGO_INCREMENTAL': '0',
                                    'CARGO_PROFILE_DEV_DEBUG': '0', 'CARGO_PROFILE_TEST_DEBUG': '0',
                                    'CARGO_PROFILE_DEV_STRIP': 'none', 'CARGO_PROFILE_TEST_STRIP': 'none'},
                    'mem_limit': '4g', 'cpus': 4, 'stop_grace_period': '2s',
                    'labels': {'dev.lumen.checkout': key, KIND: 'compiler', OWNER: str(os.getuid())}}
    for service in services.values():
        service['environment'] = {key: str(value).replace('$', '$$') for key, value in service.get('environment', {}).items()}
        service['labels'] = dict(label, **service.get('labels', {}))
        service.update(restart='no', logging={'driver': 'local', 'options': {'max-size': '8m', 'max-file': '2'}})
        service.setdefault('stop_grace_period', '45s')
    if profile.get('disposable'):
        services['postgres'].pop('volumes')
        services['postgres']['tmpfs'] = ['/var/lib/postgresql:rw,size=3g']
        services['postgres'].update(
            shm_size='1gb', mem_limit='4g', memswap_limit='4g', cpus=4,
            command=['postgres', '-c', 'shared_buffers=64MB', '-c', 'max_connections=200',
                     '-c', 'max_locks_per_transaction=256', '-c', 'max_wal_size=128MB',
                     '-c', 'min_wal_size=32MB'])
        volumes.pop('postgres')
    if source:
        services = {'builder': services['builder']}
        volumes = {name: volumes[name] for name in ('target', 'registry')}
    file = path / ('builder-compose.json' if source else 'compose.json')
    dev.write_json(file, {'name': project(profile), 'services': services, 'volumes': volumes})
    if not source and profile['build']:
        dev.runtime_links(path, profile)
    return file


def compose(path, profile, *arguments, builder=False, timeout=55):
    file = path / ('builder-compose.json' if builder else 'compose.json')
    if file.is_symlink() or json.loads(file.read_text())['name'] != project(profile):
        raise RuntimeError('Compose configuration has another owner')
    return command(profile, 'compose', '-f', str(file), '--profile', 'full', '--profile', 'build', *arguments, timeout=timeout)


def copy_source(destination, root=None):
    root = dev.source_root(root)
    expected = dev.source_identity(root)
    destination.mkdir(parents=True, exist_ok=True)
    files = {file.relative_to(root): file for file in dev.source_files(root)}
    files.update({Path('docker') / name: root / 'docker' / name
                  for name in ('local.Dockerfile', 'ytm-resolver.Dockerfile')})
    files[Path('.dockerignore')] = None
    for relative, source in files.items():
        output = destination / relative
        output.parent.mkdir(parents=True, exist_ok=True)
        contents = b'.git\n**/target\n**/__pycache__\n' if source is None else source.read_bytes()
        if not output.is_file() or output.read_bytes() != contents:
            output.write_bytes(contents)
    for path in sorted(destination.rglob('*'), reverse=True):
        if path.is_file() and path.relative_to(destination) not in files:
            path.unlink()
        elif path.is_dir() and not any(path.iterdir()):
            path.rmdir()
    if expected != dev.source_identity(root):
        raise RuntimeError('source changed while copying; current server retained; retry')
    return expected


def ensure_images(path, profile, source, descriptors, root=None):
    names = image_names(profile, root)
    missing = []
    for kind, name in names.items():
        value = dev.inspect('image', name, profile)
        if value and (value['Config'].get('Labels') or {}).get(OWNER) != str(os.getuid()):
            raise RuntimeError('local image tag has another owner; left untouched')
        if not value:
            missing.append(kind)
    if missing:
        cache = checks.runtime_root()
        handle = checks.try_lock(cache / 'locks' / ('images-' + context_key(profile) + '.lock'))
        if handle is None:
            raise RuntimeError('shared image builder is busy; retry after its active build')
        name = 'lumen-dev-' + str(os.getuid()) + '-' + context_key(profile)
        volume = 'buildx_buildkit_' + name + '0_state'
        try:
            value = dev.inspect('volume', volume, profile)
            if value and (value.get('Labels') or {}).get(OWNER) != str(os.getuid()):
                raise RuntimeError('image-builder volume has another owner')
            if not value:
                command(profile, 'volume', 'create', '--label', f'{OWNER}={os.getuid()}', '--label', f'{KIND}=buildkit', volume)
            existing = subprocess.run([*dev.docker_args(profile), 'buildx', 'inspect', name], capture_output=True, timeout=15)
            if existing.returncode:
                file = dev.state_root() / 'buildkit.toml'
                file.write_text('[worker.oci]\ngc = true\nreservedSpace = "256MB"\nmaxUsedSpace = "1GB"\nminFreeSpace = "10GB"\nmax-parallelism = 2\n')
                file.chmod(0o600)
                command(profile, 'buildx', 'create', '--name', name, '--driver', 'docker-container',
                        '--driver-opt', 'memory=1536m', '--driver-opt', 'cpu-quota=200000', '--buildkitd-config', str(file))
            for kind in missing:
                dockerfile = source / 'docker' / ('ytm-resolver.Dockerfile' if kind == 'resolver' else 'local.Dockerfile')
                target = {'compiler': 'compiler', 'core': 'runtime', 'resolver': 'runtime'}[kind]
                arguments = [*dev.docker_args(profile), 'buildx', 'build', '--builder', name, '--load', '--target', target,
                             '--label', f'{OWNER}={os.getuid()}', '--label', f'{KIND}=image', '-f', str(dockerfile), '-t', names[kind], str(source)]
                code, _, _ = checks.run_tests(arguments, dev.clean_environment(), path / 'logs' / (kind + '-image.log'),
                                              600, lambda: guard(profile), pass_fds=descriptors)
                if code:
                    raise RuntimeError(f'{kind} image build failed ({code}); current server retained')
        finally:
            try:
                subprocess.run([*dev.docker_args(profile), 'buildx', 'prune', '--builder', name, '--force', '--max-used-space', '1GB'], capture_output=True, timeout=30)
                result = subprocess.run([*dev.docker_args(profile), 'buildx', 'rm', '--keep-state', name], capture_output=True, timeout=30)
                if result.returncode:
                    print('image builder could not stop; inspect Docker before another build', flush=True)
            finally:
                handle.close()
    if not dev.inspect('image', 'postgres:18-alpine', profile):
        code, _, _ = checks.run_tests([*dev.docker_args(profile), 'pull', 'postgres:18-alpine'], dev.clean_environment(),
                                      path / 'logs/postgres-image.log', 300, lambda: guard(profile), pass_fds=descriptors)
        if code:
            raise RuntimeError('Postgres image pull failed')
    profile['images'] = names


def build(path, profile, persist=True, root=None, commit=None):
    import dev_watch
    root = dev.source_root(root)
    configure_profile(path, profile, persist=False)
    expected = identity(profile, root)
    destination = dev.builds_root(profile) / expected
    components = dev_watch.components(root)
    if not (destination / 'build.json').is_file():
        cache = checks.runtime_root()
        key = checks.cache_key(dev.REPO)
        source = cache / 'sources' / key
        source.mkdir(parents=True, exist_ok=True)
        with checks.admission(cache, key, 60) as (_, descriptors):
            source_sha = copy_source(source, root)
            ensure_images(path, profile, source, descriptors, root)
            cleanup_caches(profile)
            guard(profile)
            config(path, profile, profile['full'], source)
            old = dev.builds_root(profile) / (profile.get('build') or 'absent')
            old_document = json.loads((old / 'build.json').read_text()) if (old / 'build.json').is_file() else {}
            reuse = old_document.get('components', {}).get('rust') == components['rust'] and old_document.get('images', {}).get('compiler') == profile['images']['compiler']
            staging = dev.builds_root(profile) / ('pending-' + uuid.uuid4().hex)
            staging.mkdir(mode=0o700)
            elapsed = 0
            try:
                if reuse:
                    for name in dev.BINARIES:
                        shutil.copy2(old / name, staging / name)
                else:
                    compose(path, profile, 'up', '-d', 'builder', builder=True)
                    arguments = [*dev.docker_args(profile), 'compose', '-f', str(path / 'builder-compose.json'), 'exec', '-T', 'builder',
                                 'timeout', '--signal=TERM', '--kill-after=5s', '900s', 'cargo', 'build', '--locked', '--color', 'never', '--manifest-path', 'apps/core/Cargo.toml', '--bins']
                    print(f'building Linux source {source_sha[:12]}; two compiler jobs', flush=True)
                    code, _, elapsed = checks.run_tests(arguments, dev.clean_environment(), path / 'logs/build.log', 900,
                                                        lambda: guard(profile), pass_fds=descriptors)
                    if code:
                        raise RuntimeError(f'build failed ({code}); previous installed build retained')
                    container = compose(path, profile, 'ps', '-q', 'builder', builder=True)
                    for name in dev.BINARIES:
                        command(profile, 'cp', container + ':/target/debug/' + name, str(staging / name))
                resolver = staging / 'resolver'
                shutil.copytree(source / 'apps/ytm-resolver', resolver, ignore=shutil.ignore_patterns('tests'))
                if expected != identity(profile, root):
                    raise RuntimeError('source changed while building; previous installed build retained; retry')
                document = {'identity': expected, 'source_sha256': source_sha,
                            'components': components, 'images': profile['images'], 'architecture': profile['architecture'],
                            'created': time.time(), 'seconds': elapsed, 'source_checkout': str(dev.REPO)}
                dev.write_json(staging / 'build.json', document)
                staging.rename(destination)
            finally:
                compose(path, profile, 'rm', '--stop', '--force', 'builder', builder=True)
                if staging.exists():
                    shutil.rmtree(staging)
                profile['target_volume'] = cache_volume(profile, 'target', key)
                profile['registry_volume'] = cache_volume(profile, 'registry')
                profile['source_checkout'] = str(dev.REPO)
                touch_target(profile)
                guard(profile)
    document = json.loads((destination / 'build.json').read_text())
    for name in document['images'].values():
        value = dev.inspect('image', name, profile)
        if value and (value['Config'].get('Labels') or {}).get(OWNER) != str(os.getuid()):
            raise RuntimeError('installed build image has another owner; left untouched')
    if any(not dev.inspect('image', name, profile) for name in document['images'].values()):
        cache = checks.runtime_root()
        key = checks.cache_key(dev.REPO)
        source = cache / 'sources' / key
        source.mkdir(parents=True, exist_ok=True)
        with checks.admission(cache, key, 60) as (_, descriptors):
            copy_source(source, root)
            ensure_images(path, profile, source, descriptors, root)
    if commit and document.get('head') != commit:
        document['head'] = commit
        dev.write_json(destination / 'build.json', document)
    profile.update(build=expected, images=document['images'], resolver_snapshot=True, source_checkout=str(dev.REPO))
    if persist:
        dev.write_json(path / 'profile.json', profile)
        dev.prune_installed_builds(dev.state_root())
    print(f'installed Linux build {expected[:12]} for {profile["name"]}', flush=True)


def start_db(path, profile):
    name, volume = dev.db_names(profile)
    if not profile.get('disposable') and not dev.owned('volume', volume, profile):
        if profile.get('database_initialized') or (path / 'client-login.json').exists():
            raise RuntimeError('persistent database volume is missing; restore a backup; no empty replacement created')
        command(profile, 'volume', 'create', '--label', f'dev.lumen.local-profile={dev.label(profile)}', volume)
    previous = dev.owned('container', name, profile)
    if previous:
        bindings = previous['HostConfig'].get('PortBindings', {}).get('5432/tcp')
        expected = [{'HostIp': '127.0.0.1', 'HostPort': str(profile['ports']['postgres'])}]
        mounted = profile.get('disposable') or any(item.get('Name') == volume and item.get('Destination') == '/var/lib/postgresql' for item in previous.get('Mounts', []))
        if bindings != expected or not mounted:
            raise RuntimeError('database configuration differs; existing container left untouched')
        if previous['Config']['Labels'].get('com.docker.compose.project') != project(profile):
            raise RuntimeError('database is not managed by this profile; remove it explicitly before starting')
        if previous['State']['Running']:
            return
    dev.check_ports({'postgres': profile['ports']['postgres']})
    config(path, profile, profile['full'])
    compose(path, profile, 'up', '-d', 'postgres')
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        probe = subprocess.run([*dev.docker_args(profile), 'exec', name, 'pg_isready', '-h', '127.0.0.1', '-U', 'whio', '-d', 'whio'], capture_output=True, timeout=5)
        if not probe.returncode:
            profile['database_initialized'] = True
            dev.write_json(path / 'profile.json', profile)
            return
        time.sleep(0.2)
    raise RuntimeError('profile Postgres did not become ready within 30 seconds')


def process_states(path, profile):
    if not (path / 'compose.json').exists():
        return {}
    text = compose(path, profile, 'ps', '--all', '--format', 'json')
    rows = json.loads(text) if text.startswith('[') else [json.loads(line) for line in text.splitlines()]
    result = {}
    for row in rows:
        if row['Service'] == 'postgres':
            continue
        value = dev.inspect('container', row['ID'], profile)
        if not value or (value['Config'].get('Labels') or {}).get('dev.lumen.local-profile') != dev.label(profile):
            raise RuntimeError('app container has another owner; left untouched')
        result[row['Service']] = {'name': row['Service'], 'pid': value['State']['Pid'],
                                'is_running': value['State']['Running'], 'id': row['ID']}
    return result


def running(path, profile):
    return any(row['is_running'] for row in process_states(path, profile).values())


def wait_ready(profile):
    names = ('api', 'worker', 'youtube', 'metadata') if profile['full'] else ('api',)
    deadline = time.monotonic() + 45
    while time.monotonic() < deadline:
        if all(dev.ready(profile['ports'][name]) for name in names):
            return
        time.sleep(0.2)
    raise RuntimeError('container services did not become ready within 45 seconds; inspect just logs')


def start(path, profile, full):
    if not profile['build']:
        raise RuntimeError('profile has no build; run just up first')
    active = running(path, profile)
    if active:
        if profile['full'] != full:
            raise RuntimeError('profile is already running in another mode; stop it first')
        wait_ready(profile)
        print(f'{profile["name"]} already ready at http://127.0.0.1:{profile["ports"]["api"]}')
        return
    if profile['name'].startswith('preview-') and not profile.get('disposable'):
        active = command(profile, 'ps', '--filter', 'label=com.docker.compose.service=api', '--format', '{{.Label "dev.lumen.local-profile"}}')
        if any(value.startswith(f'{os.getuid()}-preview-') and value != dev.label(profile) for value in active.splitlines()):
            raise RuntimeError('one full preview is already active; stop it first')
    media(profile)
    guard(profile)
    names = ('api', 'worker', 'youtube', 'metadata') if full else ('api',)
    dev.check_ports({name: profile['ports'][name] for name in names})
    profile['full'] = full
    start_db(path, profile)
    config(path, profile, full)
    validate_resolvers(path, profile)
    try:
        if full:
            compose(path, profile, 'up', '-d', 'youtube', 'metadata')
        compose(path, profile, 'up', '-d', *(['worker', 'api'] if full else ['api']))
        wait_ready(profile)
    except BaseException:
        stop(path, profile, database=False)
        raise
    if profile['name'].startswith('preview-'):
        profile['expires'] = time.time() + 86400
    dev.write_json(path / 'profile.json', profile)
    print(f'{profile["name"]} ready at http://127.0.0.1:{profile["ports"]["api"]}; Docker Compose', flush=True)


def stop(path, profile, database=True):
    if (path / 'compose.json').exists():
        compose(path, profile, 'stop', *(['api', 'worker', 'youtube', 'metadata', 'postgres'] if database else ['api', 'worker', 'youtube', 'metadata']))
    print(f'{profile["name"]} stopped; database and music retained', flush=True)


def validate_resolvers(path, profile):
    directory = dev.builds_root(profile) / profile['build'] / 'resolver'
    for file in (directory / 'app').rglob('*.py'):
        try:
            compile(file.read_bytes(), str(file), 'exec')
        except SyntaxError as error:
            raise RuntimeError(f'resolver syntax error: {file.name}:{error.lineno}; previous server retained')
    if profile['full']:
        for module in ('app.main', 'app.metadata_main'):
            name = project(profile) + '-import-' + uuid.uuid4().hex[:8]
            try:
                arguments = [*dev.docker_args(profile), 'run', '--rm', '--name', name,
                             '--label', f'dev.lumen.local-profile={dev.label(profile)}', '--memory', '256m', '--cpus', '1',
                             '--user', f'{os.getuid()}:{os.getgid()}', '--mount', f'type=bind,source={directory},target=/candidate,readonly',
                             '--workdir', '/candidate', '--entrypoint', '/opt/venv/bin/python', profile['images']['resolver'], '-c', 'import ' + module]
                with (path / 'logs/resolver-import.log').open('wb') as output:
                    result = subprocess.run(arguments, stdout=output, stderr=subprocess.STDOUT, timeout=20)
                if result.returncode:
                    raise RuntimeError('resolver import failed; previous server retained; inspect profile import log')
            finally:
                remove_container(profile, name)


def replace(path, previous, candidate, schema_changed=False):
    original = (path / 'compose.json').read_bytes() if (path / 'compose.json').exists() else None
    active = running(path, previous)
    if active and not dev.ready(previous['ports']['api']):
        raise RuntimeError('current API is not ready; inspect it before replacing')
    attempted = False
    changed = []
    try:
        validate_resolvers(path, candidate)
        config(path, candidate, candidate['full'])
        old_services = json.loads(original)['services'] if original else {}
        new_services = json.loads((path / 'compose.json').read_text())['services']
        old = json.loads((dev.builds_root(previous) / previous['build'] / 'build.json').read_text())['components']
        new = json.loads((dev.builds_root(candidate) / candidate['build'] / 'build.json').read_text())['components']
        if old['rust'] != new['rust'] or previous['images']['compiler'] != candidate['images']['compiler'] or schema_changed:
            changed.extend(['worker', 'api'] if candidate['full'] else ['api'])
        if candidate['full'] and old['python'] != new['python']:
            changed.extend(['youtube', 'metadata'])
        names = ('api', 'worker', 'youtube', 'metadata') if candidate['full'] else ('api',)
        for name in names:
            if old_services.get(name) != new_services[name] and name not in changed:
                changed.append(name)
        attempted = True
        if active:
            recreate = [name for name in changed if old_services.get(name) != new_services[name]]
            restart = [name for name in changed if name not in recreate]
            if recreate:
                compose(path, candidate, 'up', '-d', '--force-recreate', *recreate)
            if restart:
                compose(path, candidate, 'restart', *restart)
            wait_ready(candidate)
        else:
            start(path, candidate, candidate['full'])
        dev.write_json(path / 'profile.json', candidate)
        create_user(path, candidate)
    except BaseException:
        if original:
            (path / 'compose.json').write_bytes(original)
        dev.runtime_links(path, previous)
        dev.write_json(path / 'profile.json', previous)
        if attempted and active and not schema_changed:
            try:
                compose(path, previous, 'up', '-d', '--force-recreate', *changed)
                wait_ready(previous)
            except BaseException as recovery:
                raise RuntimeError(f'replacement and recovery failed: {recovery}; inspect logs')
        if schema_changed:
            raise RuntimeError('migration/startup failed; backup retained; recover explicitly')
        raise
    dev.prune_installed_builds(dev.state_root())
    print(f'{candidate["name"]} updated; Postgres retained', flush=True)


def cli(path, profile, *arguments):
    result = subprocess.run([*dev.docker_args(profile), 'compose', '-f', str(path / 'compose.json'),
                            'exec', '-T', 'api', str(path / 'current-rust/whio-cli'), *arguments],
                            capture_output=True, text=True, timeout=20)
    if result.returncode:
        raise RuntimeError('local CLI failed; users and credentials retained')
    return result.stdout


def create_user(path, profile):
    file = path / 'client-login.json'
    if file.exists():
        print(f'client login: {file}')
        return
    name, _ = dev.db_names(profile)
    exists = command(profile, 'exec', name, 'psql', '-U', 'whio', '-d', 'whio', '-tAc', "select exists(select 1 from users where username='local')")
    if exists != 't':
        cli(path, profile, 'create-user', 'local', 'local')
    cli(path, profile, 'assign-admin', 'local')
    output = cli(path, profile, 'mint-app-password', 'local', 'laptop')
    password = next((line.removeprefix('password: ') for line in output.splitlines() if line.startswith('password: ')), None)
    if not password:
        raise RuntimeError('CLI did not return an app password')
    dev.write_json(file, {'server': f'http://127.0.0.1:{profile["ports"]["api"]}', 'username': 'local', 'password': password})
    print(f'client login: {file}')


def ping(profile, login):
    salt = secrets.token_hex(8)
    query = urllib.parse.urlencode({'u': login['username'], 's': salt,
        't': hashlib.md5((login['password'] + salt).encode()).hexdigest(), 'v': '1.16.1', 'c': 'dev-lifecycle', 'f': 'json'})
    with urllib.request.urlopen(f'http://127.0.0.1:{profile["ports"]["api"]}/rest/ping?{query}', timeout=5) as response:
        if json.load(response)['subsonic-response']['status'] != 'ok':
            raise RuntimeError('saved app password cannot authenticate')


def restored_preview(path, profile, directory, build_profile=None):
    document = json.loads((directory / 'backup.json').read_text())
    dump = directory / 'database.dump'
    if dev.dev_media.digest(dump) != document['dump_sha256']:
        raise RuntimeError('backup hash differs; user databases untouched')
    saved = json.loads((directory / 'profile.json').read_text())
    saved_runtime = build_profile or saved
    preview_path, preview = dev.load_profile('preview-restore-' + uuid.uuid4().hex[:8], create=True)
    preview.update({key: saved_runtime[key] for key in ('runtime', 'architecture', 'images', 'build', 'docker_context', 'media_root')})
    preview.update(credential_key=saved['credential_key'], full=False, disposable=True,
                   runner=runner_record())
    preview['rust_build'] = saved_runtime.get('rust_build', saved_runtime['build'])
    configure_profile(preview_path, preview)
    try:
        start_db(preview_path, preview)
        pg, _ = dev.db_names(preview)
        with dump.open('rb') as stream:
            result = subprocess.run([*dev.docker_args(preview), 'exec', '-i', pg, 'pg_restore', '--exit-on-error', '--no-owner', '-U', 'whio', '-d', 'whio'],
                                    stdin=stream, capture_output=True, timeout=60)
        if result.returncode:
            file = path / 'logs/backup-restore-error.log'
            file.write_bytes(result.stderr)
            file.chmod(0o600)
            raise RuntimeError(f'disposable backup restore failed; details: {file}')
        before = database_counts(preview)
        start(preview_path, preview, False)
        if (directory / 'client-login.json').exists():
            ping(preview, json.loads((directory / 'client-login.json').read_text()))
        after = database_counts(preview)
        if before != after:
            raise RuntimeError('restored library/user counts changed during candidate startup')
        print(f'disposable restore verified saved key/login and counts: {after}', flush=True)
        return after
    finally:
        destroy(preview_path, preview)


def database_counts(profile):
    name, _ = dev.db_names(profile)
    return json.loads(command(profile, 'exec', name, 'psql', '-U', 'whio', '-d', 'whio', '-tAc',
        "select json_build_object('users',(select count(*) from users),'tracks',(select count(*) from tracks),'playlists',(select count(*) from playlists),'listens',(select count(*) from listens))"))


def backup_check(path, profile):
    directories = sorted(item for item in (path / 'backups').iterdir() if (item / 'backup.json').exists())
    if not directories:
        raise RuntimeError('no completed backup; run just backup first')
    restored_preview(path, profile, directories[-1])


def destroy(path, profile):
    pg, volume = dev.db_names(profile)
    if (path / 'compose.json').exists():
        process_states(path, profile)
        compose(path, profile, 'down', '--remove-orphans')
    if (path / 'builder-compose.json').exists():
        compose(path, profile, 'down', '--remove-orphans', builder=True)
    if dev.owned('container', pg, profile):
        raise RuntimeError('owned database still exists after teardown; directory retained')
    if dev.owned('volume', volume, profile):
        command(profile, 'volume', 'rm', volume)
    shutil.rmtree(path)


def observe(path, profile, action, service):
    if action == 'services':
        print(compose(path, profile, 'ps', '--all'))
        return 0
    if action == 'logs':
        arguments = [*dev.docker_args(profile), 'compose', '-f', str(path / 'compose.json'), 'logs', '--tail', '200', '--follow']
        if service:
            arguments.append(service)
    else:
        ids = compose(path, profile, 'ps', '-q').splitlines()
        if not ids:
            raise RuntimeError('profile stopped; start it before monitoring')
        arguments = [*dev.docker_args(profile), 'stats', *ids]
    return subprocess.run(arguments, env=dev.clean_environment()).returncode


def status(path, profile):
    print(f'{profile["name"]}: runtime=docker; context={profile["docker_context"]}; build={profile["build"][:12]}; API {"ready" if dev.ready(profile["ports"]["api"]) else "stopped"}')
    print(f'url: http://127.0.0.1:{profile["ports"]["api"]}\nstate: {path}\nCompose: {path / "compose.json"}')
    if (path / 'compose.json').exists():
        print(compose(path, profile, 'ps', '--all'))
    resources(profile)


def size_bytes(value):
    match = re.fullmatch(r'([0-9.]+)\s*(B|kB|KB|MB|GB|TB|KiB|MiB|GiB)', value)
    if not match:
        raise RuntimeError('unsupported Docker storage size: ' + value)
    units = {'B': 1, 'kB': 1000, 'KB': 1000, 'MB': 1000**2, 'GB': 1000**3, 'TB': 1000**4,
             'KiB': 1024, 'MiB': 1024**2, 'GiB': 1024**3}
    return int(float(match[1]) * units[match[2]])


def measurement_image(profile):
    candidate = image_names(profile)['compiler']
    if dev.inspect('image', candidate, profile):
        return candidate
    current = profile.get('images', {}).get('compiler')
    if current and dev.inspect('image', current, profile):
        return current
    images = command(profile, 'image', 'ls', '--filter', f'label={OWNER}={os.getuid()}',
                     '--filter', f'reference=lumen-dev-{os.getuid()}/compiler:*', '--format', '{{.Repository}}:{{.Tag}}')
    if images:
        return images.splitlines()[0]
    raise RuntimeError('no owned compiler image is available for volume measurement')


def volume_filesystem_usage(profile, volume):
    name = project(profile) + '-measure-' + uuid.uuid4().hex[:8]
    script = 'filesystem=$(awk \'$2 == "/v" { print $3; exit }\' /proc/mounts); printf "%s\\n" "$filesystem"; if [ "$filesystem" = btrfs ]; then if ! command -v btrfs >/dev/null; then apt-get update -qq && apt-get install --no-install-recommends --yes btrfs-progs; fi; btrfs filesystem du -s --raw /v; fi'
    arguments = [*dev.docker_args(profile), 'run', '--rm', '--privileged', '--name', name,
                 '--label', f'dev.lumen.local-profile={dev.label(profile)}',
                 '--mount', f'type=volume,source={volume},target=/v,readonly',
                 '--entrypoint', 'sh', measurement_image(profile), '-c', script]
    try:
        output = subprocess.run(arguments, capture_output=True, text=True, timeout=30,
                                env=dev.clean_environment())
        if output.returncode:
            raise RuntimeError('cannot measure exclusive Docker volume usage')
    except (OSError, subprocess.TimeoutExpired) as error:
        raise RuntimeError('cannot measure exclusive Docker volume usage') from error
    finally:
        remove_container(profile, name)
    lines = output.stdout.splitlines()
    if not lines or not lines[0]:
        raise RuntimeError('cannot identify Docker volume filesystem')
    filesystem = lines[0]
    if filesystem != 'btrfs':
        return filesystem, None, None
    rows = [line.split() for line in lines[1:] if line.strip().endswith('/v')]
    if len(rows) != 1 or len(rows[0]) < 4:
        raise RuntimeError('unsupported btrfs exclusive usage output')
    try:
        return filesystem, int(rows[0][1]), int(rows[0][2])
    except ValueError as error:
        raise RuntimeError('unsupported btrfs exclusive usage value') from error


def storage(profile):
    document = json.loads(command(profile, 'system', 'df', '-v', '--format', '{{json .}}'))
    images = set(command(profile, 'image', 'ls', '--filter', f'label={OWNER}={os.getuid()}', '--format', '{{.ID}}', '--no-trunc').splitlines())
    image_bytes = sum(
        size_bytes(row.get('UniqueSize') or row['Size'])
        for row in document['Images'] if row['ID'] in images
    )
    volumes = []
    btrfs_targets = []
    for row in document['Volumes']:
        labels = dict(part.strip().partition('=')[::2] for part in (row['Labels'] or '').split(',') if '=' in part)
        if labels.get(OWNER) != str(os.getuid()):
            continue
        if labels.get(KIND) == 'target':
            filesystem, exclusive, shared = volume_filesystem_usage(profile, row['Name'])
            if filesystem == 'btrfs':
                btrfs_targets.append((row, labels, exclusive, shared))
                continue
        volumes.append(dict(row, cache_labels=labels, bytes=size_bytes(row['Size'])))

    if btrfs_targets:
        shared_volume, _, _, shared_bytes = max(
            btrfs_targets, key=lambda target: target[3]
        )
        volumes.extend(
            dict(
                row,
                cache_labels=labels,
                bytes=exclusive + (shared_bytes if row['Name'] == shared_volume['Name'] else 0),
            )
            for row, labels, exclusive, _ in btrfs_targets
        )

    return {'bytes': image_bytes + sum(row['bytes'] for row in volumes), 'images_unique_bytes': image_bytes,
            'volumes': volumes, 'context': profile['docker_context'], 'updated': time.time(), 'engine': document}


def guard(profile):
    result = storage(profile)
    root = checks.runtime_root()
    dev.write_json(root / ('docker-cache-' + context_key(profile) + '.json'),
                   {key: result[key] for key in ('bytes', 'context', 'updated')})
    checks.check_storage(root)
    if shutil.disk_usage(dev.STATE).free < checks.FREE_RESERVE:
        raise RuntimeError('less than 10 GiB free in the state filesystem')
    return result


def cleanup_caches(profile, retire_key=None, locked_key=None):
    root = checks.runtime_root()
    value = storage(profile)
    all_used = sum(checks.cache_size(path) for path in checks.cache_entries(root)) + value['bytes']
    for row in value['volumes']:
        labels = row['cache_labels']
        if labels.get(KIND) != 'target':
            continue
        key = labels.get('dev.lumen.checkout')
        if not key or not re.fullmatch(r'[a-f0-9]{16}', key):
            raise RuntimeError('owned compiler cache lacks a valid checkout identity')
        touched = root / ('docker-target-' + context_key(profile) + '-' + key + '.json')
        idle = time.time() - touched.stat().st_mtime if touched.exists() else 0
        if all_used <= checks.CACHE_BUDGET and idle < 86400 and key != retire_key:
            continue
        handle = checks.try_lock(root / 'locks' / (key + '.lock')) if key != locked_key else None
        if not handle and key != locked_key:
            continue
        try:
            containers = command(profile, 'ps', '--all', '--filter', f'label={OWNER}={os.getuid()}', '--filter', f'label=dev.lumen.checkout={key}', '--format', '{{.ID}}').splitlines()
            for container in containers:
                item = dev.inspect('container', container, profile)
                if item and (item['Config'].get('Labels') or {}).get(KIND) == 'compiler':
                    command(profile, 'rm', '--force', container)
            current = dev.inspect('volume', row['Name'], profile)
            if current and (current.get('Labels') or {}).get(OWNER) == str(os.getuid()):
                command(profile, 'volume', 'rm', row['Name'])
                touched.unlink(missing_ok=True)
                all_used -= row['bytes']
                print(f'removed idle Docker target {key}: {row["bytes"] / 1024**3:.2f} GiB', flush=True)
        finally:
            if handle:
                handle.close()
    referenced = set()
    for file in (dev.state_root() / 'container-builds').glob('*/*/build.json'):
        referenced.update(json.loads(file.read_text()).get('images', {}).values())
    images = command(profile, 'image', 'ls', '--filter', f'label={OWNER}={os.getuid()}', '--format', '{{.ID}} {{.Repository}}:{{.Tag}}', '--no-trunc').splitlines()
    for row in images:
        image_id, name = row.split(' ', 1)
        if name in referenced or name in image_names(profile).values():
            continue
        name = image_id if name == '<none>:<none>' else name
        value = dev.inspect('image', name, profile)
        if value and (value['Config'].get('Labels') or {}).get(OWNER) == str(os.getuid()):
            result = subprocess.run([*dev.docker_args(profile), 'image', 'rm', name], capture_output=True, timeout=30)
            if result.returncode:
                print(f'image still referenced; retained: {name}', flush=True)
    guard(profile)


def touch_target(profile):
    key = checks.cache_key(dev.REPO)
    dev.write_json(checks.runtime_root() / ('docker-target-' + context_key(profile) + '-' + key + '.json'),
                   {'checkout': str(dev.REPO), 'context': profile['docker_context'], 'updated': time.time()})


def resources(profile):
    if not profile.get('docker_context'):
        profile = dict(profile)
        configure_profile(dev.STATE / 'profiles' / profile['name'], profile, persist=False)
    result = guard(profile)
    host = sum(checks.cache_size(path) for path in checks.cache_entries(checks.runtime_root()))
    print(f'aggregate rebuildable caches: {(host + checks.docker_cache_size(checks.runtime_root())) / 1024**3:.2f} / 16 GiB')
    print(f'host targets allocated: {host / 1024**3:.2f} GiB; Docker cache/image accounting upper bound: {result["bytes"] / 1024**3:.2f} GiB')
    allocated = subprocess.check_output(['du', '-skx', str(dev.STATE)], text=True, timeout=30)
    print(f'host state allocated: {int(allocated.split()[0]) / 1024**2:.2f} GiB; free disk: {shutil.disk_usage(dev.STATE).free / 1024**3:.2f} GiB')
    print('Docker images may share layers; state excludes the mounted media; VM disk/memory and system toolchains are separate')
    print(command(profile, 'system', 'df'))


def runner_identity(pid):
    result = subprocess.run(['ps', '-o', 'lstart=', '-p', str(pid)], capture_output=True, text=True)
    if result.returncode or not result.stdout.strip():
        return None
    return result.stdout.strip()


def runner_record():
    pid = os.getpid()
    start_time = runner_identity(pid)
    if not start_time:
        raise RuntimeError('cannot identify preview runner process')
    return {'pid': pid, 'start_time': start_time}


def runner_alive(profile):
    runner = profile.get('runner')
    if not isinstance(runner, dict) or not isinstance(runner.get('pid'), int):
        return False
    start_time = runner.get('start_time')
    if not isinstance(start_time, str) or not start_time:
        return False
    identity = runner_identity(runner['pid'])
    return bool(identity) and identity == start_time


def cleanup():
    root = dev.state_root()
    contexts = {}
    for file in (root / 'profiles').glob('*/profile.json'):
        profile = json.loads(file.read_text())
        if profile.get('runtime') != 'docker' or not profile.get('docker_context'):
            continue
        contexts[profile['docker_context']] = profile
        path = file.parent
        if profile['name'].startswith(('preview-check-', 'preview-restore-')) and not runner_alive(profile):
            destroy(path, profile)
            print(f'orphaned preview removed: {profile["name"]}', flush=True)
            continue
        if not profile.get('expires') or profile['expires'] > time.time():
            continue
        watcher = checks.try_lock(path / 'watcher.lock')
        if watcher is None:
            print(f'active watcher retained: {profile["name"]}')
            continue
        key = checks.cache_key(Path(profile.get('source_checkout', str(dev.REPO))))
        build = checks.try_lock(checks.runtime_root() / 'locks' / (key + '.lock'))
        try:
            if build is None:
                print(f'active build retained: {profile["name"]}')
                continue
            destroy(path, profile)
            print(f'expired preview removed: {profile["name"]}', flush=True)
        finally:
            watcher.close()
            if build:
                build.close()
    dev.prune_installed_builds(root)
    for profile in contexts.values():
        cleanup_caches(profile)


def upgrade(path, previous, root=None, commit=None):
    import dev_watch
    root = dev.source_root(root)
    dev_watch.owner(path)
    start_db(path, previous)
    if dev_watch.migration_status(path, previous, display=False, root=root):
        raise RuntimeError('applied migration differs; restore its original source')
    candidate = dict(previous)
    build(path, candidate, persist=False, root=root, commit=commit)
    directory = dev.backup(path, previous)
    restored_preview(path, previous, directory, candidate)
    old = json.loads((dev.builds_root(previous) / previous['build'] / 'build.json').read_text())['components']
    new = json.loads((dev.builds_root(candidate) / candidate['build'] / 'build.json').read_text())['components']
    candidate['rust_build'] = previous.get('rust_build', previous['build']) if old['rust'] == new['rust'] and previous['images']['compiler'] == candidate['images']['compiler'] else candidate['build']
    replace(path, previous, candidate, schema_changed=old['migrations'] != new['migrations'])


def check(args, cargo_args, test_args):
    profile = None
    path = None
    root = None
    code = 1
    signal.signal(signal.SIGTERM, checks.interrupt)
    try:
        try:
            with dev.state_lock():
                cleanup()
        except RuntimeError as error:
            if str(error) != 'another dev setup operation is active; retry after it finishes':
                raise
            print('active lifecycle operation; expiry deferred to the next invocation', flush=True)
        path, profile = dev.load_profile('preview-check-' + uuid.uuid4().hex[:8], create=True)
        profile['disposable'] = True
        profile['source_checkout'] = str(dev.REPO)
        profile['runner'] = runner_record()
        configure_profile(path, profile)
        root = checks.runtime_root()
        key = checks.cache_key(dev.REPO)
        source = root / 'sources' / key
        source.mkdir(parents=True, exist_ok=True)
        with checks.admission(root, key, args.queue_timeout) as (_, descriptors):
            copy_source(source)
            ensure_images(path, profile, source, descriptors)
            cleanup_caches(profile)
            config(path, profile, False, source)
            if args.postgres:
                start_db(path, profile)
            compose(path, profile, 'up', '-d', 'builder', builder=True)
            arguments = [*dev.docker_args(profile), 'compose', '-f', str(path / 'builder-compose.json'), 'exec', '-T']
            if args.postgres:
                arguments.extend(('--env', f'DATABASE_URL=postgres://whio:{profile["postgres_password"]}@postgres:5432/whio'))
            if os.environ.get('WHIO_TEST_WORKER_PRODUCTION_INTERVALS'):
                arguments.extend(('--env', 'WHIO_TEST_WORKER_PRODUCTION_INTERVALS=1'))
            arguments.extend(('builder', 'timeout', '--signal=TERM', '--kill-after=5s', f'{args.timeout}s', 'cargo', 'test', '--locked', '--color', 'never', '--manifest-path', 'apps/core/Cargo.toml'))
            if args.postgres:
                arguments.extend(('--features', 'postgres-integration'))
            arguments.extend((*cargo_args, '--', f'--test-threads={args.threads}', *test_args))
            print(f'check runtime=docker; mode={"postgres" if args.postgres else "unit"}; two Cargo jobs; timeout={args.timeout}s', flush=True)
            code, executed, elapsed = checks.run_tests(arguments, dev.clean_environment(), path / 'logs/check.log',
                                                       args.timeout, lambda: guard(profile), pass_fds=descriptors)
            print(f'executed={executed}; elapsed={elapsed:.2f}s; exit={code}', flush=True)
            if not code and not executed:
                code = 2
                print('no tests executed; selection rejected', flush=True)
            touch_target(profile)
    except KeyboardInterrupt:
        code = 130
    except checks.Interrupted as error:
        code = 128 + error.signum
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        print(f'container check failed: {error}', file=sys.stderr)
        code = 1
    finally:
        if profile:
            if root and path:
                for log in (path / 'logs').glob('*.log'):
                    shutil.copy2(log, root / 'logs' / (profile['name'] + '-' + log.name))
            saved = signal.signal(signal.SIGTERM, signal.SIG_IGN)
            try:
                if (path / 'builder-compose.json').exists():
                    compose(path, profile, 'down', '--remove-orphans', builder=True)
                if (path / 'compose.json').exists():
                    destroy(path, profile)
                elif path.exists():
                    shutil.rmtree(path)
            finally:
                signal.signal(signal.SIGTERM, saved)
    return code
