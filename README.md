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

`WHIO_DATABASE_URL` is required. Core connects to PostgreSQL and runs the
embedded migrations before it binds its HTTP listener. If the database is
unavailable or migrations are incompatible, Core exits without serving
requests.

For the local Compose setup, the value is configured automatically. When
running Core directly against the development database, use:

```text
WHIO_DATABASE_URL=postgres://whio:whio-dev@localhost:5432/whio
```

YTM is disabled by default. set `WHIO_YOUTUBE_ENABLED=true` to enable it.

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
