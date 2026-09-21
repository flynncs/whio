# whio

better subsonic streaming with instant YTM resolution.

```text
apps/core/          rust core
apps/ytm-resolver/  optional YTM resolver
contracts/          cross-service contracts
```

## resolver api generation

the openapi contract generates the python models and rust client.

```sh
./tools/generate-resolver-contract.sh
```

check for drift with:

```sh
./tools/generate-resolver-contract.sh --check
```

don't edit `apps/resolver-client-generated/` or
`apps/ytm-resolver/app/generated/resolver_v1.py` by hand.

## core config

- `WHIO_DATABASE_URL`: required. postgres url. embedded migrations run before core binds.
- `WHIO_CREDENTIAL_KEY`: required. base64 of 32 random bytes (`openssl rand -base64 32`). encrypts client credential secrets at rest.
- `WHIO_BIND_ADDRESS`: defaults to `127.0.0.1:3000`
- `WHIO_LOG_LEVEL`: defaults to `info`
- `WHIO_YOUTUBE_ENABLED`: off by default
- `WHIO_YOUTUBE_RESOLVER_URL`, `WHIO_YOUTUBE_RESOLVER_CONNECT_TIMEOUT_SECONDS`, `WHIO_YOUTUBE_RESOLVER_TOTAL_TIMEOUT_SECONDS`: resolver target and budgets when youtube is enabled
- `WHIO_PLAYBACK_MAX_SESSIONS`: defaults to `16`. concurrent spool playbacks before overload
- `WHIO_SPOOL_DIR`: defaults to the system temp dir. stale `whio-*.part` files are purged at startup
- `WHIO_YOUTUBE_MEDIA_SUFFIX`: defaults to `googlevideo.com`. host suffix accepted for resolved YouTube media
- `WHIO_ARTWORK_HOST_SUFFIXES`: comma-separated, defaults to `googleusercontent.com,ytimg.com`. public artwork hosts core will fetch and proxy
- `WHIO_DEDUP_MODE`: defaults to `suppress`. substitute an available local track for a conservatively matched YTM search result. `shadow` logs decisions without substitution; `off` skips matching. saved track IDs and source associations are unchanged.
- `WHIO_SOURCE_TAGS`: defaults to `off`. `artist` prefixes each search result's artist with the provider it came from (`[ND]`, `[YTM]`). display only, and nothing is stored

local compose sets everything except the credential key, which lives in a
gitignored `.env` next to the compose files (`openssl rand -base64 32`).

## docker

```sh
docker compose up -d
```

whio runs on `localhost:3000`.

## cli

```sh
cargo install --path apps/core --bin whio-cli
whio-cli create-user flynn "flynn"
whio-cli mint-app-password flynn feishin
whio-cli revoke <credential-id>
```

reads `.env` in the repo root; flags override it. reinstall after cli changes.
