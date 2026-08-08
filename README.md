# whio

better subsonic streaming with instant YTM resolution.

```text
apps/core/          rust core
apps/backend/       python proof of concept
apps/ytm-resolver/  optional YTM resolver
contracts/          cross-service contracts
```

the python app stays in place until the rust core proves the same behaviour.

## Generated resolver DTOs

The Rust resolver wire models are generated from the OpenAPI contract. After installing
OpenAPI Generator, regenerate them with:

```sh
./tools/generate-resolver-dto.sh
```

Do not edit files under `apps/core/src/resolver/generated/` by hand. Keep domain types,
the resolver client, and mappings in handwritten Rust code.
