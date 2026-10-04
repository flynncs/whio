# development

requires Python 3, Just, Git, Docker Compose/Buildx and Watchexec.

## setup

set `LUMEN_MEDIA_ROOT` to your music directory. mounted read-only.

```sh
export LUMEN_MEDIA_ROOT=/absolute/path/to/music
just up personal
```

up builds the current source the first time, starts the full stack and creates a
user; the credential file location is printed. each profile has its own database
and key.

| profile | API | worker health | YouTube | metadata | Postgres |
| --- | --- | --- | --- | --- | --- |
| personal | 3000 | 3001 | 8000 | 8100 | 5432 |
| dev | 3100 | 3101 | 18000 | 18100 | 55432 |
| preview-* | allocated | allocated | allocated | allocated | allocated |

- personal: persistent listening; updated with `just deploy`.
- dev: separate database; follows source changes while watching.
- preview-*: temporary app for an isolated worktree; 24-hour lease.
- ports bind to loopback. omitted profile means personal.

the stack is api, worker, both resolvers and Postgres; modes are not selectable.

```sh
just down personal
```

## changes

```sh
just up dev
just dev dev
```

- Rust saves rebuild and restart API/worker.
- Python saves check imports and restart resolvers.
- saves during a build queue one further build.
- build/import failures leave the previous server running.
- Postgres stays up. Ctrl-C stops the watcher, leaving the app running.
- migration/dependency changes pause watching.

```sh
just migrations dev
just deploy
```

migrations reports applied and pending SQLx migrations. deploy backs up
personal, verifies the restore and swaps the running app. applied migrations are
not automatically rolled back on failure.

## logs and status

```sh
just
just status dev
just logs dev
just logs dev api
just logs dev postgres
just down dev
```

`just` lists every profile on one line; `just status <profile>` adds its Docker
resource accounting. services also appear in OrbStack/Docker Desktop. Ctrl-C
closes the log viewer without stopping services. build/import logs live under the
profile's `logs/`.

## data and limits

state: `~/Library/Application Support/lumen-dev` on macOS,
`~/.local/state/lumen-dev` on Linux/WSL2. override with `LUMEN_DEV_STATE`.
contains keys, client credentials, builds, logs and backups; keep it out of Git.
Postgres data lives in labelled Docker volumes and survives stop/start.

```sh
just backup personal
just clean
python3 tools/dev-cache.py
python3 tools/dev-cache.py --prune
```

- backup restores into a disposable database before keeping the archive.
- clean preserves personal/dev data and expires inactive previews.
- active builds/watchers prevent preview expiry; one full preview at a time.
- builds share compiler images/registry; each checkout has a separate target cache.
- managed caches/images: 16 GiB guard; host free space: 10 GiB reserve.
- guards are checked periodically; they are not disk quotas.
- prune removes idle owned caches. raw Cargo/Compose bypass these controls.

| service | memory cap | CPU cap |
| --- | --- | --- |
| Postgres | 512 MiB | 1 |
| API / worker, each | 512 MiB | 2 |
| each resolver | 256 MiB | 1 |
| compiler, while building | 4 GiB | 4 |

logs rotate at 8 MiB, two files. artwork/spool budgets: 256/512 MiB.
container memory excludes engine VM overhead. music is outside the build budget.

## agent worktrees

supply local `AGENTS.md` instructions and a task file with the accepted base,
allowed writes, contracts, dependencies and focused checks. exclude local
instructions/planning files from Git.

```sh
python3 tools/agent_worktree.py create task-name --packet path/to/task.md --base HEAD --source head
python3 tools/agent_worktree.py status task-name
```

- `head`: committed base.
- `current`: snapshot of tracked/untracked source; preserves the original index.
  use `HEAD current` for uncommitted dependencies.
- instructions and local planning context are copied; ignored credentials and
  build artifacts are excluded.
- run [focused tests](TESTING.md) inside the returned checkout.

for an app preview, run these inside that checkout:

```sh
just up preview-task-name
just dev preview-task-name
```

retire with `python3 tools/agent_worktree.py retire task-name`. it refuses
uncommitted source, extra ignored files and active checks/watchers. removes owned
previews and the checkout's target cache; retains the branch and shared
images/registry.

## remote music copy — macOS

requires rsync, SSH and Python 3 on the source. selection file:

```json
{"selected_complete_album_directories": [{"album": "artist/album"}]}
```

```sh
just media import --remote user@host --source /srv/music --manifest /path/to/selection.json
just media verify
```

copies complete albums, capped at 5 GiB; checks sizes/hashes and mounts the copy
read-only. without `LUMEN_MEDIA_ROOT`, macOS profiles use this copy.
Linux/WSL2 uses `LUMEN_MEDIA_ROOT` instead.

## self-hosting launcher

[`compose.yaml`](CONFIGURATION.md) runs published images with its own state and
no database volume shared with a managed profile. check port conflicts before
starting it alongside one.
