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

local compose sets everything except the credential key, which lives in a
gitignored `.env` next to the compose files (`openssl rand -base64 32`).

## docker

```sh
docker compose up -d
```

whio runs on `localhost:3000`.

for development:

```sh
docker compose -f compose.dev.yaml up --build
```

The development Compose setup starts PostgreSQL 18 with a persistent
`postgres-data` volume and exposes it on `localhost:5432`. Core uses the
`postgres` service hostname inside Compose. The initial Core-owned migration
is in `apps/core/migrations/0001_track_identity.sql`.
