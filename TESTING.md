# testing

run the affected module or integration target during development:

```sh
just test features::music_library::domain
just test-db features::music_library::storage::postgres::test_delivery
just test-it native_artist_info_http
```

| location | coverage | runner |
| --- | --- | --- |
| `apps/core/src/` test modules | Rust domain/service/storage behaviour | unit or Postgres, depending on feature |
| `apps/core/tests/integration/` | HTTP, playback, wiring and spawned-worker integration | the `integration` target, filtered by module |
| `apps/core/tests/integration/support/` | shared credentials, media and worker fixtures | imported by integration tests |
| `apps/ytm-resolver/tests/` | resolver/provider contracts and controlled responses | pytest |
| `apps/core/src/integrations/*/fixtures/` | native-provider response fixtures, including malformed/absent fields | consumed by provider tests |
| `tools/tests/` | dev lifecycle, watching, Docker ownership, worktrees and check admission | unittest |

Rust unit checks need no database. Postgres checks enable `postgres-integration`
and create a disposable server per run, with full migrations and SQLx database
isolation. they never select personal/dev databases. fixtures cover malformed provider data and retry/failure paths.

target filters and runner options are available directly:

```sh
python3 tools/test-postgres.py --docker -- --test integration artist_info_http
python3 tools/test-core.py --docker -- --lib features::metadata::freshness
```

scoped runners require a library filter or named target and reject zero executed
tests. omit `--docker` for native Cargo checks. each checkout has one heavy
execution slot; the machine/user has two. builds use two Cargo jobs. excess
commands print queue progress and fail after 60 seconds; `--queue-timeout 0`
fails immediately when busy. use separate worktrees for concurrent workers.

output streams live and is retained up to 16 MiB per check log. the default
execution deadline is 600 seconds; `--timeout` changes it. test threads default
to two; `--threads` accepts 1–4. poll running jobs with short waits and retain a
progressing run instead of restarting it.

Postgres test containers use a 512 MiB memory filesystem, 768 MiB memory cap
and two CPUs, without a persistent volume. success/failure/timeout/interruption
removes the owned environment. surviving children retain execution locks;
leased Docker resources are reclaimed once inactive. memory-backed runs do not
prove disk persistence or VM recovery. CI uses disk-backed Postgres.

## Python checks

```sh
python3 -m unittest discover -s tools/tests -v
cd apps/ytm-resolver
uv run --with pytest --python 3.14 python -m pytest tests -q
```

tool tests use temporary state, mocked subprocesses and temporary Git repositories.

## combined gates

full suites: CI or the combined integration gate.

```sh
python3 tools/test-core.py --docker --full
python3 tools/test-postgres.py --docker --full
cargo fmt --manifest-path apps/core/Cargo.toml --all -- --check
cargo clippy --locked --manifest-path apps/core/Cargo.toml --all-targets -- -D warnings
```

[CI](.github/workflows/checks.yml) defines the authoritative commands, including
worker-process tests, contract drift and resolver pytest. full native CI tests
use `cargo test --locked --manifest-path apps/core/Cargo.toml`, adding
`--features postgres-integration` and a disposable `DATABASE_URL` for DB coverage.

## generated contracts

`contracts/resolver-v1.openapi.yaml` generates the Python models and Rust client.
install openapi-generator 7.24.0 or set `OPENAPI_GENERATOR_JAR` to that version's
jar, plus Java, uv and Cargo. generate changes through the source contract:

```sh
./tools/generate-resolver-contract.sh
./tools/generate-resolver-contract.sh --check
./tools/generate-subsonic-types.sh --check
```

do not edit `apps/resolver-client-generated/`,
`apps/ytm-resolver/app/generated/resolver_v1.py` or Subsonic generated types by hand.
