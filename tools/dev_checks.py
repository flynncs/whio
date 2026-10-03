import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import selectors
import signal
import shutil
from contextlib import contextmanager
import subprocess
import sys
import tempfile
import time
import uuid


class Interrupted(Exception):
    def __init__(self, signum):
        self.signum = signum


def interrupt(signum, frame):
    raise Interrupted(signum)


def docker(*args):
    return subprocess.check_output(
        ["docker", *args], text=True, stderr=subprocess.STDOUT, timeout=30
    ).strip()


def start_postgres(name, slot=None):
    slot_label = []
    if slot is not None:
        owner = hashlib.sha256(platform.node().encode()).hexdigest()[:16]
        label = f'dev.lumen.test-slot={owner}-{os.getuid()}-{slot}'
        previous = docker('ps', '--all', '--filter', f'label={label}', '--format', '{{.Names}}')
        for stale in previous.splitlines():
            if not re.fullmatch(r'lumen-test-[a-f0-9]{12}', stale):
                raise RuntimeError('unexpected container in owned test slot')
            cleanup_postgres(stale)
        slot_label = ['--label', label]
    docker(
        "run", "--detach", "--rm", "--pull=never", "--name", name,
        "--label", "dev.lumen.test-runtime=true", "--memory", "768m",
        *slot_label,
        "--memory-swap", "768m", "--cpus", "2",
        "--tmpfs", "/var/lib/postgresql:rw,size=512m",
        "--publish", "127.0.0.1::5432",
        "--env", "POSTGRES_USER=whio", "--env", "POSTGRES_DB=whio",
        "--env", "POSTGRES_PASSWORD=whio-test",
        "postgres:18-alpine", "-c", "shared_buffers=32MB",
    )
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        probe = subprocess.run(
            ["docker", "exec", name, "pg_isready", "-h", "127.0.0.1", "-U", "whio", "-d", "whio"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5,
        )
        if probe.returncode == 0:
            binding = docker("port", name, "5432/tcp")
            if not re.fullmatch(r"127\.0\.0\.1:\d+", binding):
                raise RuntimeError("test postgres must bind only to IPv4 loopback")
            port = binding.rsplit(":", 1)[1]
            return f"postgres://whio:whio-test@127.0.0.1:{port}/whio"
        time.sleep(0.2)
    raise RuntimeError("test postgres did not become ready within 30 seconds")


def cleanup_postgres(name):
    removed = subprocess.run(
        ["docker", "rm", "--force", name], stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL, timeout=30,
    )
    if removed.returncode == 0:
        return
    remaining = docker("ps", "--all", "--filter", f"name=^{name}$", "--format", "{{.Names}}")
    if remaining:
        raise RuntimeError(f"container cleanup failed: {name}")


def stop_process_group(process):
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        process.wait(timeout=5)
        return
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        pass
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait(timeout=5)


def run_tests(command, environment, log_path, timeout, guard=None, pass_fds=()):
    started = time.monotonic()
    process = subprocess.Popen(
        command, env=environment, stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT, start_new_session=True,
        pass_fds=pass_fds,
    )
    executed = 0
    pending = b""
    log_bytes = 0
    log_truncated = False
    next_guard = started + STORAGE_CHECK_INTERVAL
    try:
        with selectors.DefaultSelector() as events, log_path.open("wb") as log:
            events.register(process.stdout, selectors.EVENT_READ)
            while events.get_map():
                if guard and time.monotonic() >= next_guard:
                    guard()
                    next_guard = time.monotonic() + STORAGE_CHECK_INTERVAL
                remaining = timeout - (time.monotonic() - started)
                if remaining <= 0:
                    print(
                        f"\ntest command exceeded {timeout}s; stopping its process group",
                        flush=True,
                    )
                    return 124, executed, time.monotonic() - started
                for key, _ in events.select(min(1, remaining)):
                    chunk = os.read(key.fd, 65536)
                    if not chunk:
                        events.unregister(key.fileobj)
                        continue
                    retained = chunk[:max(0, MAX_LOG_BYTES - log_bytes)]
                    log.write(retained)
                    log_bytes += len(retained)
                    if len(retained) < len(chunk) and not log_truncated:
                        log.write(b'\nsaved log truncated at its size limit; terminal output continues\n')
                        log_truncated = True
                    log.flush()
                    sys.stdout.buffer.write(chunk)
                    sys.stdout.buffer.flush()
                    pending += chunk
                    lines = pending.split(b"\n")
                    pending = lines.pop()[-65536:]
                    for line in lines:
                        result = re.search(
                            rb"test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed", line
                        )
                        if result:
                            executed += int(result[1]) + int(result[2])
            remaining = max(0, timeout - (time.monotonic() - started))
            try:
                code = process.wait(timeout=remaining)
            except subprocess.TimeoutExpired:
                return 124, executed, time.monotonic() - started
            return code if code >= 0 else 128 - code, executed, time.monotonic() - started
    finally:
        stop_process_group(process)
        process.stdout.close()


CACHE_BUDGET = 16 * 1024 ** 3
FREE_RESERVE = 10 * 1024 ** 3
MAX_LOG_BYTES = 16 * 1024 ** 2
STORAGE_CHECK_INTERVAL = 15


def runtime_root():
    root = Path('/tmp') / f'lumen-checks-{os.getuid()}'
    root.mkdir(mode=0o700, exist_ok=True)
    if root.is_symlink() or root.stat().st_uid != os.getuid():
        raise RuntimeError('runtime directory must belong to the current user')
    for name in ('locks', 'targets', 'logs'):
        directory = root / name
        directory.mkdir(mode=0o700, exist_ok=True)
        if directory.is_symlink():
            raise RuntimeError('runtime subdirectory must not be a symlink')
    return root


def cache_key(repo):
    return hashlib.sha256(str(repo.resolve()).encode()).hexdigest()[:16]


def try_lock(path):
    handle = path.open('a+')
    try:
        fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        handle.close()
        return None
    return handle


@contextmanager
def admission(root, key, timeout):
    held = []
    deadline = time.monotonic() + timeout
    next_report = 0
    try:
        while True:
            checkout = try_lock(root / 'locks' / f'{key}.lock')
            if checkout:
                slot = None
                for index in range(2):
                    slot = try_lock(root / 'locks' / f'slot-{index}.lock')
                    if slot:
                        break
                if slot:
                    held = [checkout, slot]
                    yield index, tuple(handle.fileno() for handle in held)
                    return
                checkout.close()
            if time.monotonic() >= deadline:
                raise RuntimeError('local check capacity busy; admission deadline reached; retry after an active run finishes')
            if time.monotonic() >= next_report:
                print('waiting for local check capacity (two runs per machine, one per checkout)', flush=True)
                next_report = time.monotonic() + 5
            time.sleep(min(0.2, max(0, deadline - time.monotonic())))
    finally:
        for handle in reversed(held):
            handle.close()


def cache_entries(root):
    entries = []
    for path in (root / 'targets').iterdir():
        if path.is_symlink() or not re.fullmatch(r'[a-f0-9]{16}', path.name):
            continue
        try:
            if path.is_dir():
                entries.append((path.stat().st_mtime, path))
        except FileNotFoundError:
            pass
    return [path for _, path in sorted(entries)]


def cache_size(path):
    total = 0
    seen = set()
    pending = [path]
    while pending:
        directory = pending.pop()
        try:
            with os.scandir(directory) as entries:
                for entry in entries:
                    try:
                        metadata = entry.stat(follow_symlinks=False)
                        identity = (metadata.st_dev, metadata.st_ino)
                        if identity not in seen:
                            total += metadata.st_blocks * 512
                            seen.add(identity)
                        if entry.is_dir(follow_symlinks=False):
                            pending.append(Path(entry.path))
                    except FileNotFoundError:
                        pass
        except FileNotFoundError:
            pass
    return total


def check_storage(root):
    used = sum(cache_size(path) for path in cache_entries(root)) + docker_cache_size(root)
    free = shutil.disk_usage(root).free
    if used > CACHE_BUDGET:
        raise RuntimeError('aggregate managed build caches exceeded 16 GiB; run just cleanup when idle')
    if free < FREE_RESERVE:
        raise RuntimeError('less than 10 GiB free; reclaim build/storage space before continuing')


def docker_cache_size(root):
    return sum(json.loads(file.read_text())['bytes'] for file in root.glob('docker-cache-*.json'))


def prune_caches(root, remove_all=False):
    maintenance = try_lock(root / 'locks' / 'maintenance.lock')
    deadline = time.monotonic() + 5
    while maintenance is None:
        if time.monotonic() >= deadline:
            raise RuntimeError('cache maintenance stayed busy for five seconds; retry shortly')
        time.sleep(0.05)
        maintenance = try_lock(root / 'locks' / 'maintenance.lock')
    try:
        entries = cache_entries(root)
        used = sum(cache_size(path) for path in entries) + docker_cache_size(root)
        for path in entries:
            if not remove_all and used <= CACHE_BUDGET:
                break
            handle = try_lock(root / 'locks' / f'{path.name}.lock')
            if handle is None:
                continue
            try:
                size = cache_size(path)
                shutil.rmtree(path)
                used -= size
                print(f'removed idle build cache {path.name}: {size / 1024 ** 3:.2f} GiB', flush=True)
            finally:
                handle.close()
        if not remove_all and used > CACHE_BUDGET:
            raise RuntimeError('active build caches exceed the retention budget; stop new checks until they finish')
        logs = sorted((root / 'logs').glob('*.log'), key=lambda path: path.stat().st_mtime)
        for path in logs[:-20]:
            if path.stat().st_mtime < time.time() - 86400:
                path.unlink()
    finally:
        maintenance.close()


def selected_args(parser, values, full):
    if values[:1] == ['--']:
        values = values[1:]
    separator = values.index('--') if '--' in values else len(values)
    cargo_args, test_args = values[:separator], values[separator + 1:]
    targets = 0
    library = False
    filters = 0
    index = 0
    while index < len(cargo_args):
        value = cargo_args[index]
        if value == '--lib' and not library:
            library = True
        elif value == '--test' and index + 1 < len(cargo_args):
            index += 1
            if not re.fullmatch(r'[A-Za-z0-9_][A-Za-z0-9_-]*', cargo_args[index]):
                parser.error('name an individual integration target after --test')
            targets += 1
        elif value and not value.startswith('-') and filters == 0:
            filters += 1
        else:
            parser.error(f'unsupported selector: {value}; use --lib <filter> or --test <name>')
        index += 1
    index = 0
    while index < len(test_args):
        value = test_args[index]
        if value in ('--exact', '--nocapture', '--show-output', '--ignored', '--include-ignored'):
            pass
        elif value == '--skip' and index + 1 < len(test_args):
            index += 1
        else:
            parser.error(f'unsupported test option: {value}')
        index += 1
    if not full and (not (targets or library) or (library and not filters)):
        parser.error('workers must select --lib <filter> or --test <name>; complete suites require integration-owner --full')
    return cargo_args, test_args


def main(postgres_default=False):
    parser = argparse.ArgumentParser(description='run scoped core checks with bounded local resources')
    parser.add_argument('--postgres', action='store_true', default=postgres_default)
    parser.add_argument('--docker', action='store_true')
    parser.add_argument('--full', action='store_true')
    parser.add_argument('--threads', type=int, choices=range(1, 5), default=2)
    parser.add_argument('--timeout', type=int, default=600)
    parser.add_argument('--queue-timeout', type=int, default=60)
    parser.add_argument('cargo_args', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if args.timeout <= 0 or not 0 <= args.queue_timeout <= 60:
        parser.error('execution timeout must be positive; queue timeout must be 0–60 seconds')
    cargo_args, test_args = selected_args(parser, args.cargo_args, args.full)
    if args.docker:
        import dev_docker
        return dev_docker.check(args, cargo_args, test_args)
    repo = Path(__file__).resolve().parent.parent
    command = ['cargo', 'test', '--locked', '--color', 'never', '--manifest-path', str(repo / 'apps/core/Cargo.toml')]
    if args.postgres:
        command.extend(['--features', 'postgres-integration'])
    command.extend([*cargo_args, '--', f'--test-threads={args.threads}', *test_args])
    signal.signal(signal.SIGTERM, interrupt)
    code = 1
    try:
        root = runtime_root()
        key = cache_key(repo)
        with admission(root, key, args.queue_timeout) as (slot, lock_fds):
            prune_caches(root)
            check_storage(root)
            target = root / 'targets' / key
            target.mkdir(exist_ok=True)
            os.utime(target)
            descriptor, path = tempfile.mkstemp(prefix=f'{key}-', suffix='.log', dir=root / 'logs')
            os.close(descriptor)
            log_path = Path(path)
            environment = os.environ.copy()
            environment.update({
                'CARGO_TARGET_DIR': str(target), 'CARGO_BUILD_JOBS': '2',
                'CARGO_INCREMENTAL': '0', 'CARGO_PROFILE_DEV_DEBUG': '0',
                'CARGO_PROFILE_TEST_DEBUG': '0',
                'CARGO_PROFILE_DEV_STRIP': 'none', 'CARGO_PROFILE_TEST_STRIP': 'none',
            })
            environment.pop('DATABASE_URL', None)
            environment.pop('WHIO_DATABASE_URL', None)
            name = f'lumen-test-{uuid.uuid4().hex[:12]}' if args.postgres else None
            try:
                print(f'check mode={"postgres" if args.postgres else "unit"}; threads={args.threads}; timeout={args.timeout}s', flush=True)
                print(f'build cache: {target}\nlog: {log_path}', flush=True)
                if name:
                    environment['DATABASE_URL'] = start_postgres(name, slot)
                code, executed, elapsed = run_tests(
                    command, environment, log_path, args.timeout,
                    lambda: check_storage(root), pass_fds=lock_fds,
                )
                check_storage(root)
                print(f'\nexecuted={executed}; elapsed={elapsed:.2f}s; exit={code}', flush=True)
                if code == 0 and executed == 0:
                    print('no tests executed; check the selected target or filter', file=sys.stderr)
                    code = 2
            finally:
                if name:
                    saved_handler = signal.signal(signal.SIGTERM, signal.SIG_IGN)
                    try:
                        cleanup_postgres(name)
                    finally:
                        signal.signal(signal.SIGTERM, saved_handler)
                os.utime(target)
    except KeyboardInterrupt:
        code = 130
    except Interrupted as error:
        code = 128 + error.signum
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        print(f'check runner failed: {error}', file=sys.stderr)
        code = 1
    return code
