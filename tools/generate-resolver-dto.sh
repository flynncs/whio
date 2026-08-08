#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "$script_dir/.." && pwd)"
contract="$repo_root/contracts/resolver-v1.openapi.yaml"
generated_root="$repo_root/apps/core/src/resolver/generated"

if ! command -v openapi-generator >/dev/null 2>&1; then
  printf '%s\n' 'openapi-generator is required. Install it before running this script.' >&2
  exit 1
fi

temporary_root="$(mktemp -d "${TMPDIR:-/private/tmp}/whio-resolver-codegen.XXXXXX")"
cleanup() {
  rm -rf "$temporary_root"
}
trap cleanup EXIT

openapi-generator validate -i "$contract"

openapi-generator generate \
  -i "$contract" \
  -g rust \
  -o "$temporary_root" \
  --global-property 'models=CatalogueSearchRequest:CatalogueSearchResponse:CatalogueCandidate:SourceIdentity:ErrorResponse:ErrorCode,supportingFiles,modelDocs=false,modelTests=false' \
  --additional-properties 'packageName=whio_resolver_dto,packageVersion=0.1.0,hideGenerationTimestamp=true'

rm -rf "$generated_root"
mkdir -p "$generated_root"
cp "$temporary_root/src/models/"*.rs "$generated_root/"

for model_file in "$generated_root"/*.rs; do
  sed -i '' 's/use crate::models;/use crate::resolver::generated as models;/' "$model_file"
done

sed -i '' '1i\
#![allow(unused_imports, clippy::derivable_impls, clippy::empty_docs)]\
' "$generated_root/mod.rs"
cargo fmt --manifest-path "$repo_root/apps/core/Cargo.toml" --all

printf 'Generated resolver DTOs in %s\n' "$generated_root"
