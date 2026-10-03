# development

requires Python 3, Just, Git, Docker Compose/Buildx and Watchexec.

## setup

set `LUMEN_MEDIA_ROOT` to your music directory. mounted read-only.

```sh
export LUMEN_MEDIA_ROOT=/absolute/path/to/music
just setup personal
just start personal
just login personal
```

setup builds the current source. start creates a user; login prints the
credential file location. each profile has its own database and key.

| profile | API | worker health | YouTube | metadata | Postgres |
| --- | --- | --- | --- | --- | --- |
| personal | 3000 | 3001 | 8000 | 8100 | 5432 |
| dev | 3100 | 3101 | 18000 | 18100 | 55432 |
| preview-* | allocated | allocated | allocated | allocated | allocated |

- personal: persistent listening; updated with `just upgrade personal`.
- dev: separate database; follows source changes while watching.
- preview-*: temporary app for an isolated worktree; 24-hour lease.
- ports bind to loopback. omitted profile means personal.
- local mode: API and Postgres. full mode adds worker and both resolvers.

```sh
just stop personal
just full personal
```

stop before switching modes. `just local personal` selects local mode;
`just start personal` resumes the saved mode.

## changes

```sh
just setup dev
just watch dev
```

- Rust saves rebuild and restart API/worker.
- Python saves check imports and restart resolvers.
- saves during a build queue one further build.
- build/import failures leave the previous server running.
- Postgres stays up. Ctrl-C stops the watcher, leaving the app running.
- migration/dependency changes pause watching until `just migrate dev`.

```sh
just reload dev
just migrations dev
just migrate dev
just upgrade personal
```

reload applies source changes manually. migrate backs up before applying
pending migrations. personal upgrades also test a restored database first.
applied migrations are not automatically rolled back on failure.

## logs and status

```sh
just status dev
just services dev
just logs dev api
just logs dev postgres
just monitor dev
just resources dev
just stop dev
```

services also appear in OrbStack/Docker Desktop. Ctrl-C closes log/stat viewers
without stopping services. build/import logs live under the profile's `logs/`.

## data and limits

state: `~/Library/Application Support/lumen-dev` on macOS,
`~/.local/state/lumen-dev` on Linux/WSL2. override with `LUMEN_DEV_STATE`.
contains keys, client credentials, builds, logs and backups; keep it out of Git.
Postgres data lives in labelled Docker volumes and survives stop/start.

```sh
just backup personal
just backup-check personal
just cleanup
python3 tools/dev-cache.py
python3 tools/dev-cache.py --prune
```

- backup-check restores into a disposable database.
- cleanup preserves personal/dev data and expires inactive previews.
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
just agent-create task-name path/to/task.md HEAD head
just agent-status task-name
```

- `head`: committed base.
- `current`: snapshot of tracked/untracked source; preserves the original index.
  use `HEAD current` for uncommitted dependencies.
- instructions and local planning context are copied; ignored credentials and
  build artifacts are excluded.
- run [focused tests](TESTING.md) inside the returned checkout.

for an app preview, run these inside that checkout:

```sh
just setup preview-task-name
just full preview-task-name
just watch preview-task-name
```

retire with `just agent-retire task-name`. it refuses uncommitted source,
extra ignored files and active checks/watchers. removes owned previews and the
checkout's target cache; retains the branch and shared images/registry.

## remote music copy — macOS

requires rsync, SSH and Python 3 on the source. selection file:

```json
{"selected_complete_album_directories": [{"album": "artist/album"}]}
```

```sh
just media-import --remote user@host --source /srv/music --manifest /path/to/selection.json
just media-verify
```

copies complete albums, capped at 5 GiB; checks sizes/hashes and mounts the copy
read-only. without `LUMEN_MEDIA_ROOT`, macOS profiles use this copy.
Linux/WSL2 uses `LUMEN_MEDIA_ROOT` instead.

## other launchers

| launcher | setup |
| --- | --- |
| `compose.yaml` | published images; [configuration](CONFIGURATION.md) |
| `compose.dev.yaml`, `compose.homelab-dev.yaml` | source builds with Cargo Watch; set `WHIO_MEDIA_PATH` and credential key in `.env` |
| native fallback | `python3 tools/dev.py setup <profile> --runtime native`; also needs Cargo, uv, Process Compose |

these launchers have independent state; check port conflicts before starting.
native apps appear in Process Compose, Postgres in Docker.
`just containerize <profile>` converts a native profile after backup/restore checks.
