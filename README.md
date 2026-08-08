# whio

better subsonic streaming with instant YTM resolution.

```text
apps/core/          rust core
apps/backend/       python proof of concept
apps/ytm-resolver/  optional YTM resolver
contracts/          cross-service contracts
```

the python app stays in place until the rust core proves the same behaviour.

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
