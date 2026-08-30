#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "$script_dir/.." && pwd)"
contract="$repo_root/contracts/subsonic/openapi.json"
generated="$repo_root/apps/subsonic-types-generated"
mode="${1:-generate}"
expected_openapi_generator_version="7.24.0"

case "$mode" in
  generate | --check) ;;
  *)
    printf 'usage: %s [--check]\n' "$0" >&2
    exit 2
    ;;
esac

if command -v openapi-generator >/dev/null 2>&1; then
  openapi_codegen=(openapi-generator)
elif [[ -n "${OPENAPI_GENERATOR_JAR:-}" ]]; then
  openapi_codegen=(java -jar "$OPENAPI_GENERATOR_JAR")
else
  printf '%s\n' 'openapi-generator is required. or set OPENAPI_GENERATOR_JAR.' >&2
  exit 1
fi

actual_openapi_generator_version="$("${openapi_codegen[@]}" version)"
if [[ "$actual_openapi_generator_version" != "$expected_openapi_generator_version" ]]; then
  printf 'openapi generator %s is required. found %s.\n' \
    "$expected_openapi_generator_version" \
    "$actual_openapi_generator_version" >&2
  exit 1
fi

temporary_root="$(mktemp -d)"
cleanup() {
  rm -rf "$temporary_root"
}
trap cleanup EXIT

# the generator emits only src/models; the crate shell is handwritten and
# carried along so formatting and diffing see the final tree
staging="$temporary_root/crate"
cp -R "$generated/." "$staging/"
openapi_generator_log="$temporary_root/openapi-generator.log"

"${openapi_codegen[@]}" validate -i "$contract"

if ! "${openapi_codegen[@]}" generate \
  --quiet \
  --skip-validate-spec \
  -i "$contract" \
  -g rust \
  -o "$staging/gen" \
  --global-property 'models,supportingFiles,modelDocs=false,modelTests=false,apiDocs=false,apiTests=false' \
  --additional-properties 'packageName=whio-subsonic-api,packageVersion=0.1.0,hideGenerationTimestamp=true' \
  >"$openapi_generator_log" 2>&1; then
  cat "$openapi_generator_log" >&2
  exit 1
fi

rm -rf "$staging/src/models"
mkdir -p "$staging/src"
mv "$staging/gen/src/models" "$staging/src/models"
rm -rf "$staging/gen"

cargo fmt --manifest-path "$staging/Cargo.toml" --all

if [[ "$mode" == "--check" ]]; then
  if ! diff -ru "$generated" "$staging"; then
    printf '%s\n' 'generated subsonic types are out of date. run tools/generate-subsonic-types.sh.' >&2
    exit 1
  fi

  printf '%s\n' 'generated subsonic types are up to date.'
  exit 0
fi

rm -rf "$generated/src/models"
cp -R "$staging/src/models" "$generated/src/models"
cargo fmt --manifest-path "$repo_root/apps/core/Cargo.toml" --all

printf 'generated rust models in %s\n' "$generated/src/models"
