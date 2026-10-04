# configuration

## container setup

`compose.yaml`: core API, worker, Postgres, YouTube resolver, metadata resolver.
API on port 3000; other services on the internal network.
Postgres volume: `postgres-data`.

required in `.env`: `WHIO_CREDENTIAL_KEY`, 32 random bytes encoded as base64
(`openssl rand -base64 32`). keep the same key with the database; it encrypts
client credentials.

```sh
docker compose up -d
docker compose ps
docker compose logs -f core
```

create a user and issue a client password:

```sh
docker compose exec core whio-cli create-user listener "listener"
docker compose exec core whio-cli mint-app-password listener feishin
```

use that username and the printed password in a Subsonic client at
`http://localhost:3000`. the CLI also supports `revoke <credential-id>` and
`assign-admin <username>`. to run the CLI on the host instead, install it with
`cargo install --path apps/core --bin whio-cli`; it reads the repository `.env`.
`--database-url` and `--credential-key` override those values.

`docker compose down` stops services and retains the database volume.
for managed local builds, see [development](DEVELOPMENT.md).

## local music

mount music read-only and set the matching container path.
`compose.override.yaml`:

```yaml
services:
  core:
    environment:
      WHIO_MEDIA_ROOTS: /mnt/music
    volumes:
      - /absolute/host/music:/mnt/music:ro
```

the API scans configured libraries at startup. roots are comma-separated
absolute paths, optionally labelled: `local=/mnt/music,archive=/mnt/archive`.
roots sharing a label form one library. saved associations use those paths;
keep container mount paths stable between restarts.

## core environment

| variable | default / requirement | use |
| --- | --- | --- |
| `WHIO_DATABASE_URL` | required | Postgres connection; embedded migrations run before binding |
| `WHIO_CREDENTIAL_KEY` | required | base64-encoded 32-byte encryption key |
| `WHIO_BIND_ADDRESS` | `127.0.0.1:3000` | API listener; container image sets `0.0.0.0:3000` |
| `WHIO_LOG_LEVEL` | `info` | tracing level |
| `WHIO_MEDIA_ROOTS` | absent | local library roots |
| `WHIO_YOUTUBE_ENABLED` | off | enable YouTube resolution |
| `WHIO_YOUTUBE_RESOLVER_URL` | required when enabled | absolute HTTP(S) resolver URL |
| `WHIO_METADATA_RESOLVER_URL` | absent, disabled | independent metadata resolver URL |
| `WHIO_DEDUP_MODE` | `suppress` | substitute conservative local matches; `shadow` logs, `off` skips |
| `WHIO_SOURCE_TAGS` | `off` | `artist` adds provider prefixes to search display; no stored changes |
| `WHIO_PLAYBACK_MAX_SESSIONS` | `16` | concurrent spool playbacks before overload |
| `WHIO_SPOOL_DIR` | temp directory / `whio-spools` | playback files; stale parts are purged at startup |
| `WHIO_YOUTUBE_MEDIA_SUFFIX` | `googlevideo.com` | accepted resolved-media host suffix |
| `WHIO_ARTWORK_HOST_SUFFIXES` | `googleusercontent.com,ytimg.com` | comma-separated public artwork host suffixes |
| `WHIO_METADATA_STORE_BUDGET_BYTES` | `33554432` | metadata store budget; minimum 1 MiB |
| `WHIO_METADATA_LANGUAGE` | `en` | metadata language tag |
| `WHIO_ARTWORK_CACHE_DIR` | absent | enable ready artwork storage at the chosen path |
| `WHIO_ARTWORK_CACHE_BYTES` | `2147483648` | artwork cache budget when configured |

resolver connect/total budgets use
`WHIO_YOUTUBE_RESOLVER_CONNECT_TIMEOUT_SECONDS` /
`WHIO_YOUTUBE_RESOLVER_TOTAL_TIMEOUT_SECONDS`, and the corresponding
`WHIO_METADATA_RESOLVER_*` variables. defaults are 2/35 seconds. total must be at
least 35 seconds and greater than connect.

`WHIO_SPOOL_MAX_BYTES` bounds one spool; `WHIO_SPOOL_BUDGET_BYTES` bounds their
combined storage. max must not exceed the total budget. managed development
profiles set smaller storage budgets than the core defaults.

`WHIO_METADATA_ARTIST_PROFILE` / `WHIO_METADATA_SOURCE_ORDER`: profile and
per-field provider order. accepted values: [config.rs](apps/core/src/app/config.rs).

## worker

core and worker share the database and credential key. metadata jobs need the
metadata resolver URL in the worker too. configured queues in Compose are
`whio.jobs.probe,whio.metadata.refresh,whio.listening.delivery`.

| variable | default | use |
| --- | --- | --- |
| `WHIO_WORKER_QUEUES` | empty | comma-separated queues; required for worker startup |
| `WHIO_WORKER_BIND_ADDRESS` | `127.0.0.1:3001` | health listener |
| `WHIO_WORKER_MAX_WORKERS` | `4` | concurrent jobs |
| `WHIO_WORKER_POLL_INTERVAL_MILLISECONDS` | `200` | ready-job polling |
| `WHIO_WORKER_PROMOTE_INTERVAL_MILLISECONDS` | `250` | scheduled-job promotion |
| `WHIO_WORKER_DRAIN_TIMEOUT_SECONDS` | `30` | graceful shutdown budget |

Compose allows 45 seconds to stop the worker so the drain budget can complete.

## metadata providers

set these inside the metadata resolver container, using a Compose override or
its environment. values in a host `.env` are not forwarded automatically.

| variable | use |
| --- | --- |
| `WHIO_RESOLVER_AUDIODB_KEY` | enables TheAudioDB |
| `WHIO_RESOLVER_MUSICBRAINZ_USER_AGENT` | identifies the client and enables MusicBrainz |
| `WHIO_RESOLVER_WIKIMEDIA_USER_AGENT` | optional separate Wikimedia client identity |
| `WHIO_RESOLVER_LISTENBRAINZ_TOKEN` | enables ListenBrainz when MusicBrainz is also configured |
| `WHIO_METADATA_ALGORITHM` | ListenBrainz recommendation algorithm |

core reads cached metadata; the worker runs provider refreshes.
