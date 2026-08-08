#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "$script_dir/.." && pwd)"
contract="$repo_root/contracts/resolver-v1.openapi.yaml"
python_root="$repo_root/apps/ytm-resolver"
python_generated="$python_root/app/generated/resolver_v1.py"
rust_generated="$repo_root/apps/resolver-client-generated"
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

if [[ -x "$python_root/.venv/bin/datamodel-codegen" ]]; then
  python_codegen=("$python_root/.venv/bin/datamodel-codegen")
elif command -v uv >/dev/null 2>&1; then
  python_codegen=(uv run --project "$python_root" --frozen datamodel-codegen)
else
  printf '%s\n' 'uv or the ytm-resolver virtual environment is required.' >&2
  exit 1
fi

temporary_root="$(mktemp -d "${TMPDIR:-/private/tmp}/whio-resolver-codegen.XXXXXX")"
cleanup() {
  rm -rf "$temporary_root"
}
trap cleanup EXIT

rust_staging="$temporary_root/rust"
python_staging="$temporary_root/resolver_v1.py"
openapi_generator_log="$temporary_root/openapi-generator.log"

"${openapi_codegen[@]}" validate -i "$contract"

if ! "${openapi_codegen[@]}" generate \
  --quiet \
  --skip-validate-spec \
  -i "$contract" \
  -g rust \
  -o "$rust_staging" \
  --global-property 'modelDocs=false,modelTests=false,apiDocs=false,apiTests=false' \
  --additional-properties 'packageName=whio-resolver-api,packageVersion=0.1.0,hideGenerationTimestamp=true,library=reqwest,reqwestDefaultFeatures=,useSerdePathToError=true' \
  >"$openapi_generator_log" 2>&1; then
  cat "$openapi_generator_log" >&2
  exit 1
fi

(
  cd "$python_root"
  "${python_codegen[@]}" --output "$python_staging"
)

cargo fmt --manifest-path "$rust_staging/Cargo.toml" --all

if [[ "$mode" == "--check" ]]; then
  differences=0

  diff -ru "$rust_staging/Cargo.toml" "$rust_generated/Cargo.toml" || differences=1
  diff -ru "$rust_staging/src" "$rust_generated/src" || differences=1
  diff -u "$python_staging" "$python_generated" || differences=1

  if [[ "$differences" -ne 0 ]]; then
    printf '%s\n' 'generated resolver files are out of date. run tools/generate-resolver-contract.sh.' >&2
    exit 1
  fi

  printf '%s\n' 'generated resolver files are up to date.'
  exit 0
fi

rm -rf "$rust_generated"
mkdir -p "$rust_generated"
cp "$rust_staging/Cargo.toml" "$rust_generated/Cargo.toml"
cp -R "$rust_staging/src" "$rust_generated/src"
cp "$python_staging" "$python_generated"

cargo fmt --manifest-path "$repo_root/apps/core/Cargo.toml" --all

printf 'generated python models in %s\n' "$python_generated"
printf 'generated rust client in %s\n' "$rust_generated"
