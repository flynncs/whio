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

YTM is disabled by default. set `WHIO_YOUTUBE_ENABLED=true` to enable it.
