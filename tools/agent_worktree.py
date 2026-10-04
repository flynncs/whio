import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time

import dev
import dev_checks as checks


def task_name(value):
    if not re.fullmatch(r'[a-z][a-z0-9-]{0,31}', value):
        raise ValueError('task names must be lowercase letters, digits and hyphens, starting with a letter')
    return value


def git(*arguments, cwd=None, env=None, input=None):
    result = subprocess.run(['git', *arguments], cwd=cwd or dev.REPO, env=env, input=input,
                            capture_output=True, timeout=30)
    if result.returncode:
        raise RuntimeError(f'git {arguments[0]} failed: {result.stderr.decode().strip()}')
    return result.stdout


def file_identity(path):
    if path.is_symlink():
        return hashlib.sha256(os.readlink(path).encode()).hexdigest()
    return hashlib.sha256(path.read_bytes()).hexdigest() if path.exists() else 'deleted'


def working_identity():
    names = git('ls-files', '--cached', '--others', '--exclude-standard', '-z').decode().split('\0')
    result = hashlib.sha256()
    for name in sorted(set(filter(None, names))):
        result.update(name.encode() + b'\0' + file_identity(dev.REPO / name).encode() + b'\0')
    return result.hexdigest()


def context_files(packet):
    packet = packet.resolve()
    if not packet.is_relative_to(dev.REPO) or not packet.is_file():
        raise ValueError('packet must be an existing file in this checkout')
    files = [dev.REPO / 'AGENTS.md', packet]
    for directory in ('docs/agents', 'docs/areas'):
        files.extend(path for path in (dev.REPO / directory).rglob('*') if path.is_file())
    unique = sorted(set(files))
    for path in unique:
        if path.is_symlink() or any(part in ('.env', 'credentials', 'secrets') for part in path.relative_to(dev.REPO).parts):
            raise RuntimeError('context contains a symlink or secret path')
    if sum(path.stat().st_size for path in unique) > 25 * 1024 ** 2:
        raise RuntimeError('context exceeds 25 MiB; supply a bounded context packet')
    return unique


def snapshot(base):
    head = git('rev-parse', 'HEAD').decode().strip()
    if base != head:
        raise ValueError('current-source snapshot requires the current HEAD as its base')
    index_path = Path(git('rev-parse', '--path-format=absolute', '--git-path', 'index').decode().strip())
    index_before = file_identity(index_path)
    source_before = working_identity()
    with tempfile.TemporaryDirectory(prefix='lumen-snapshot-') as temporary:
        env = os.environ.copy()
        env['GIT_INDEX_FILE'] = str(Path(temporary) / 'index')
        env.update({'GIT_AUTHOR_NAME': 'lumen dev setup', 'GIT_AUTHOR_EMAIL': 'dev-setup@example.invalid',
                    'GIT_COMMITTER_NAME': 'lumen dev setup', 'GIT_COMMITTER_EMAIL': 'dev-setup@example.invalid'})
        git('read-tree', base, env=env)
        git('add', '--all', '--', '.', env=env)
        tree = git('write-tree', env=env).decode().strip()
        revision = git('commit-tree', tree, '-p', base, env=env,
                       input=b'laptop agent source snapshot\n').decode().strip()
    if file_identity(index_path) != index_before or working_identity() != source_before:
        raise RuntimeError('source or real Git index changed during snapshot; no worktree created')
    return revision, source_before


def create(name, packet, base, source):
    task_name(name)
    root = dev.state_root()
    agents = root / 'agents'
    worktrees = root / 'worktrees'
    for directory in (agents, worktrees):
        if directory.is_symlink():
            raise RuntimeError('agent state directory must not be a symlink')
        directory.mkdir(mode=0o700, exist_ok=True)
    marker = agents / f'{name}.json'
    path = worktrees / name
    if marker.exists() or path.exists():
        raise RuntimeError('agent name/path already exists; inspect it before creating another')
    files = context_files(packet)
    revision = git('rev-parse', '--verify', '--end-of-options', base + '^{commit}').decode().strip()
    source_hash = None
    if source == 'current':
        revision, source_hash = snapshot(revision)
    for required in ('tools/test-core.py', 'tools/test-postgres.py', 'tools/dev_checks.py'):
        result = subprocess.run(['git', 'cat-file', '-e', f'{revision}:{required}'], cwd=dev.REPO, capture_output=True)
        if result.returncode:
            raise RuntimeError('base lacks the local check runners; choose --source current or a revision containing them')
    branch = f'codex/{name}'
    git('worktree', 'add', '-b', branch, str(path), revision)
    record = {'name': name, 'uid': os.getuid(), 'path': str(path), 'branch': branch,
              'base': base, 'revision': revision, 'source': source, 'source_sha256': source_hash,
              'packet': str(packet.resolve().relative_to(dev.REPO)), 'created': time.time(), 'context': {}}
    dev.write_json(marker, record)
    for origin in files:
        relative = origin.relative_to(dev.REPO)
        destination = path / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(origin, destination)
        record['context'][str(relative)] = file_identity(destination)
    ignored = git('check-ignore', 'AGENTS.md', record['packet'], cwd=path).decode().splitlines()
    if ignored != ['AGENTS.md', record['packet']]:
        raise RuntimeError('supplied instructions/packet must remain locally excluded; worktree retained for inspection')
    dev.write_json(marker, record)
    print(f'agent checkout: {path}\nbranch: {branch}\nsource: {source} {revision}\npacket: {path / record["packet"]}')
    print('run assigned tools/test-core.py or tools/test-postgres.py checks in this checkout; no full suites')


def load(name):
    task_name(name)
    root = dev.state_root()
    marker = root / 'agents' / f'{name}.json'
    if not marker.is_file() or marker.is_symlink():
        raise RuntimeError('agent checkout is not owned by this setup')
    record = json.loads(marker.read_text())
    path = Path(record['path'])
    if record['uid'] != os.getuid() or path != root / 'worktrees' / name or path.is_symlink():
        raise RuntimeError('agent checkout ownership differs')
    return marker, path, record


def dirty_context(path, record):
    files = git('ls-files', '--others', '--ignored', '--exclude-standard', '-z', cwd=path).decode().split('\0')
    for name in filter(None, files):
        if name in record['context'] and file_identity(path / name) == record['context'][name]:
            continue
        if '__pycache__' in Path(name).parts and name.endswith('.pyc'):
            continue
        return name
    for name, identity in record['context'].items():
        if file_identity(path / name) != identity:
            return name
    return None


def retire(name):
    marker, path, record = load(name)
    if git('status', '--porcelain', cwd=path):
        raise RuntimeError('agent checkout has changes; retirement refused')
    changed = dirty_context(path, record)
    if changed:
        raise RuntimeError(f'agent checkout has changed or extra ignored files ({changed}); retirement refused')
    root = checks.runtime_root()
    key = checks.cache_key(path)
    lock = checks.try_lock(root / 'locks' / f'{key}.lock')
    if lock is None:
        raise RuntimeError('agent checkout has an active check/build; retirement refused')
    try:
        import dev_docker
        contexts = {}
        for file in (dev.state_root() / 'profiles').glob('*/profile.json'):
            profile = json.loads(file.read_text())
            if profile.get('runtime') != 'docker':
                continue
            contexts[profile['docker_context']] = profile
            if profile.get('source_checkout') != str(path):
                continue
            if not profile['name'].startswith('preview-'):
                raise RuntimeError('persistent profile depends on this checkout; retirement refused')
            watcher = checks.try_lock(file.parent / 'watcher.lock')
            if watcher is None:
                raise RuntimeError('agent preview has an active watcher; retirement refused')
            try:
                dev_docker.destroy(file.parent, profile)
            finally:
                watcher.close()
        for profile in contexts.values():
            dev_docker.cleanup_caches(profile, retire_key=key, locked_key=key)
        git('worktree', 'remove', str(path))
        cache = root / 'targets' / key
        if cache.exists() and not cache.is_symlink():
            shutil.rmtree(cache)
        marker.unlink()
    finally:
        lock.close()
    print(f'retired {name}; branch {record["branch"]} retained')


def main():
    parser = argparse.ArgumentParser(description='isolated agent checkouts with explicit source/context handoff')
    parser.add_argument('command', choices=('create', 'status', 'retire'))
    parser.add_argument('name', type=task_name)
    parser.add_argument('--packet', type=Path)
    parser.add_argument('--base', default='HEAD')
    parser.add_argument('--source', choices=('head', 'current'), default='head')
    args = parser.parse_args()
    if args.command == 'create' and not args.packet:
        parser.error('create requires --packet with concrete allowed writes and checks')
    try:
        with dev.state_lock():
            if args.command == 'create':
                create(args.name, args.packet, args.base, args.source)
            elif args.command == 'retire':
                retire(args.name)
            else:
                _, path, record = load(args.name)
                print(f'{path}\nbranch: {record["branch"]}; source: {record["source"]} {record["revision"]}')
                print(git('status', '--short', cwd=path).decode(), end='')
        return 0
    except (OSError, ValueError, KeyError, RuntimeError, subprocess.SubprocessError) as error:
        print(f'agent checkout failed: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
