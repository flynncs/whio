# configuration

## container setup

`compose.yaml`: core API, worker, Postgres and YouTube resolver.
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
| `WHIO_TRUSTED_PROXIES` | empty | comma-separated proxy IP addresses trusted for `x-forwarded-for`; without this, clients behind the homelab proxy share its IP throttle key |
| `WHIO_AUTH_FAILURE_BURST` | `10` | failed authentication attempts allowed per account or client IP before throttling |
| `WHIO_AUTH_FAILURE_REPLENISH_SECONDS` | `60` | interval for replenishing one failed-authentication allowance |
| `WHIO_LOG_FORMAT` | `text` | log output format: `text` or `json` |
| `WHIO_LOG` | absent | tracing filter directives; takes precedence over `WHIO_LOG_LEVEL` |
| `WHIO_LOG_LEVEL` | `info` | fallback tracing filter level |
| `WHIO_MEDIA_ROOTS` | absent | local library roots |
| `WHIO_YOUTUBE_ENABLED` | off | enable YouTube resolution |
| `WHIO_YOUTUBE_RESOLVER_URL` | required when enabled | absolute HTTP(S) resolver URL |
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

Set `WHIO_TRUSTED_PROXIES` to the homelab proxy's IP address when requests
reach Whio through that proxy; only trusted proxies' `x-forwarded-for` headers
are used to identify client IPs.

YouTube resolver connect/total budgets use
`WHIO_YOUTUBE_RESOLVER_CONNECT_TIMEOUT_SECONDS` /
`WHIO_YOUTUBE_RESOLVER_TOTAL_TIMEOUT_SECONDS`. defaults are 2/35 seconds. total
must be at least 35 seconds and greater than connect.

`WHIO_SPOOL_MAX_BYTES` bounds one spool; `WHIO_SPOOL_BUDGET_BYTES` bounds their
combined storage. max must not exceed the total budget. managed development
profiles set smaller storage budgets than the core defaults.

`WHIO_METADATA_ARTIST_PROFILE` / `WHIO_METADATA_SOURCE_ORDER`: profile and
per-field provider order. accepted values: [config.rs](apps/core/src/app/config.rs).

## worker

core and worker share the database and credential key. metadata jobs require at
least one enabled document provider, available by default through MusicBrainz.
configured queues in Compose are
`whio.jobs.probe,whio.metadata.refresh,whio.listening.delivery`.

| variable | default | use |
| --- | --- | --- |
| `WHIO_WORKER_QUEUES` | empty | comma-separated queues; required for worker startup |
| `WHIO_WORKER_BIND_ADDRESS` | `127.0.0.1:3001` | health listener |
| `WHIO_WORKER_MAX_WORKERS` | `4` | concurrent jobs |
| `WHIO_WORKER_POLL_INTERVAL_MILLISECONDS` | `200` | ready-job polling |
| `WHIO_WORKER_PROMOTE_INTERVAL_MILLISECONDS` | `250` | scheduled-job promotion |
| `WHIO_WORKER_DRAIN_TIMEOUT_SECONDS` | `30` | graceful shutdown budget; used only when the millisecond setting is absent |
| `WHIO_WORKER_DRAIN_TIMEOUT_MILLISECONDS` | `30000` | graceful shutdown budget; takes precedence over the seconds setting |
| `WHIO_WORKER_HEARTBEAT_RESCUE_INTERVAL_MILLISECONDS` | `30000` | stale-heartbeat rescue scan interval |
| `WHIO_WORKER_HEARTBEAT_INTERVAL_MILLISECONDS` | Awa default (`30000`) | optional heartbeat write interval |
| `WHIO_WORKER_HEARTBEAT_STALENESS_MILLISECONDS` | Awa default (`90000`) | optional stale-heartbeat threshold; configure at least three times the heartbeat interval |
| `WHIO_WORKER_RETRY_BASE_DELAY_MILLISECONDS` | `1000` | base unit for the exponential retry policy; attempt `n` starts at `2^n` times this value |
| `WHIO_WORKER_RETRY_MAX_DELAY_MILLISECONDS` | `86400000` | maximum retry delay |

Compose allows 45 seconds to stop the worker so the drain budget can complete.

## metadata providers

provider settings are read by core and worker. Set the same values in both
services; the managed dev launcher forwards mode-0600 `providers.env` settings
to those services only. Every enabled variable accepts exactly `true` or
`false`. Secret values can be supplied directly or through the corresponding
`_FILE` variable; do not set both.

| variable | default / requirement | use |
| --- | --- | --- |
| `WHIO_MUSICBRAINZ_ENABLED` | `true` | enable MusicBrainz, Wikimedia and Wikipedia document lookups |
| `WHIO_MUSICBRAINZ_USER_AGENT` | platform user agent | MusicBrainz client identity |
| `WHIO_WIKIMEDIA_USER_AGENT` | MusicBrainz user agent | optional separate Wikimedia client identity |
| `WHIO_AUDIODB_ENABLED` | `false` | enable TheAudioDB documents |
| `WHIO_AUDIODB_KEY` / `WHIO_AUDIODB_KEY_FILE` | required when enabled | TheAudioDB API key |
| `WHIO_LISTENBRAINZ_METADATA_ENABLED` | `false` | enable ListenBrainz artist documents; does not affect scrobbling |
| `WHIO_LISTENBRAINZ_TOKEN` / `WHIO_LISTENBRAINZ_TOKEN_FILE` | required when enabled | ListenBrainz API token |
| `WHIO_LISTENBRAINZ_ALGORITHM` | pinned session-based default | ListenBrainz similar-artist algorithm |

core reads cached metadata; the worker runs provider refreshes.
