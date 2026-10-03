# whio

better subsonic streaming with instant YTM resolution.

Rust core, Postgres, optional Python resolvers. use your local music library
alongside YouTube Music in a Subsonic client.

## run

create a `.env` with a credential key:

```sh
printf 'WHIO_CREDENTIAL_KEY=%s\n' "$(openssl rand -base64 32)" > .env
docker compose up -d
```

connect to `http://localhost:3000`. [setup](CONFIGURATION.md) covers user
creation, client passwords, music mounts and configuration. keep the key with
your database; reuse it on subsequent starts.

## develop

[development](DEVELOPMENT.md) covers personal/dev servers, watching files,
migrations, logs, resource limits and agent worktrees.
[testing](TESTING.md) covers focused checks, fixtures and the CI gates.
