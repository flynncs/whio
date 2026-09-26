#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd -- "$script_dir/.." && pwd)"
tasks=""

usage() {
  printf 'usage: %s [--root DIR] [--tasks PATH]\n' "$0"
  printf '\nstructurally validates contracts/compat/manifest.json against\n'
  printf 'contracts/compat/manifest.schema.json and the pinned conformance counts.\n'
  printf '\n  --root DIR    repository root holding contracts/compat (default: this script parent)\n'
  printf '  --tasks PATH  task ledger that owner_task must resolve against: a markdown\n'
  printf '                table or a JSON inventory. defaults to\n'
  printf '                <root>/docs/implementation-tasks.md or .json when either exists.\n'
  printf '                when no ledger is available the embedded ledger ID set is used\n'
  printf '                and an explicit skipped-validation warning is printed.\n'
  printf '\na bare PATH argument is accepted as the ledger path.\n'
  printf 'exit status: 0 valid, 1 invalid manifest, 2 usage or internal error\n'
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h | --help)
      usage
      exit 0
      ;;
    --root)
      if [[ $# -lt 2 ]]; then
        usage >&2
        exit 2
      fi
      if ! root="$(cd -- "$2" && pwd)"; then
        printf 'cannot use root directory: %s\n' "$2" >&2
        exit 2
      fi
      shift 2
      ;;
    --root=*)
      if ! root="$(cd -- "${1#--root=}" && pwd)"; then
        printf 'cannot use root directory: %s\n' "${1#--root=}" >&2
        exit 2
      fi
      shift
      ;;
    --tasks)
      if [[ $# -lt 2 ]]; then
        usage >&2
        exit 2
      fi
      tasks="$2"
      shift 2
      ;;
    --tasks=*)
      tasks="${1#--tasks=}"
      shift
      ;;
    -*)
      usage >&2
      exit 2
      ;;
    *)
      if [[ -n "$tasks" ]]; then
        usage >&2
        exit 2
      fi
      tasks="$1"
      shift
      ;;
  esac
done

if ! command -v python3 >/dev/null 2>&1; then
  printf '%s\n' 'python3 is required.' >&2
  exit 2
fi

if [[ ! -r "$root/contracts/compat/manifest.json" || ! -r "$root/contracts/compat/manifest.schema.json" ]]; then
  printf 'unreadable manifest under %s\n' "$root/contracts/compat" >&2
  exit 2
fi

exec python3 - "$root" "$tasks" <<'PY'
import json
import os
import re
import sys

root = sys.argv[1]
requested_tasks = sys.argv[2]
manifest_path = root + "/contracts/compat/manifest.json"
schema_path = root + "/contracts/compat/manifest.schema.json"

LEDGER_TASKS = {
    "F01", "F02", "F03", "F04", "F90",
    "L01", "L02", "L03", "L04", "L05", "L06", "L07", "L08", "L09", "L10", "L11", "L90",
    "S01", "S02", "S03", "S04", "S05", "S06", "S07", "S08", "S09", "S10", "S11", "S90",
    "J01", "J02", "J03", "J90",
    "E01", "E02", "E03", "E04", "E05", "E06", "E07", "E08", "E09", "E10", "E11", "E90",
    "A01", "A02", "A03", "A04", "A05", "A06", "A07", "A08", "A09", "A10", "A90",
    "V01", "V02", "V03", "V04", "V05", "V06", "V07", "V08", "V09", "V10", "V11", "V12",
    "V13", "V14", "V15", "V90",
    "N01", "N02", "N03", "N04", "N05", "N06", "N07", "N90",
}

TASK_ID = re.compile(r"^[FLSJEAVN][0-9]{2}[a-z]?$")

PINNED_COUNTS = {
    "subsonic": 48,
    "navidrome": 30,
    "retained_uncalled": 2,
    "field_mappings": 132,
    "sort_mappings": 53,
    "filter_translations": 21,
    "operators": 5,
    "navidrome_compatibility_profiles": 8,
    "opensubsonic_extensions": 6,
}

FORBIDDEN_KEYS = re.compile(r"^(percent|percentage|pct|completion|completion_date|completed_at|complete|done|done_at)$")

errors = []


def fail(message):
    errors.append(message)


try:
    with open(manifest_path, encoding="utf-8") as handle:
        manifest = json.load(handle)
except (OSError, ValueError) as error:
    print("error: cannot read manifest: %s" % error, file=sys.stderr)
    sys.exit(2)

try:
    with open(schema_path, encoding="utf-8") as handle:
        schema = json.load(handle)
except (OSError, ValueError) as error:
    print("error: cannot read schema: %s" % error, file=sys.stderr)
    sys.exit(2)

if not isinstance(schema, dict):
    print("error: schema must be a JSON object", file=sys.stderr)
    sys.exit(2)


def markdown_ledger_ids(text):
    ids = set()
    for line in text.splitlines():
        if not line.lstrip().startswith("|"):
            continue
        for cell in line.strip().strip("|").split("|"):
            candidate = cell.strip().strip("`").strip()
            if TASK_ID.match(candidate):
                ids.add(candidate)
    return ids


def json_ledger_ids(text):
    ids = set()

    def collect(node):
        if isinstance(node, dict):
            for key, value in node.items():
                if key == "id" and isinstance(value, str) and TASK_ID.match(value):
                    ids.add(value)
                collect(value)
        elif isinstance(node, list):
            for item in node:
                collect(item)
        elif isinstance(node, str) and TASK_ID.match(node):
            ids.add(node)

    collect(json.loads(text))
    return ids


def ledger_ids_from(path):
    with open(path, encoding="utf-8") as handle:
        text = handle.read()
    if path.endswith(".json") or text.lstrip().startswith("{"):
        return json_ledger_ids(text)
    return markdown_ledger_ids(text)


ledger_path = None
if requested_tasks:
    if os.path.isfile(requested_tasks):
        ledger_path = requested_tasks
    else:
        print(
            "warning: task ledger %s not found; owner_task validated against the embedded ledger ID set"
            % requested_tasks,
            file=sys.stderr,
        )
else:
    for candidate in (
        root + "/docs/implementation-tasks.md",
        root + "/docs/implementation-tasks.json",
    ):
        if os.path.isfile(candidate):
            ledger_path = candidate
            break
    if ledger_path is None:
        print(
            "warning: no task ledger supplied or found under %s/docs; owner_task validated against the embedded ledger ID set"
            % root,
            file=sys.stderr,
        )

if ledger_path is not None:
    try:
        ledger = ledger_ids_from(ledger_path)
    except (OSError, ValueError) as error:
        print("error: cannot read task ledger %s: %s" % (ledger_path, error), file=sys.stderr)
        sys.exit(2)
    if not ledger:
        print("error: no task IDs found in %s" % ledger_path, file=sys.stderr)
        sys.exit(2)
else:
    ledger = LEDGER_TASKS

if schema.get("$schema") != "https://json-schema.org/draft/2020-12/schema":
    fail("schema is not draft 2020-12")

defs = schema.get("$defs", {})


def resolve(ref):
    if not ref.startswith("#/"):
        raise ValueError("unsupported $ref %s" % ref)
    node = schema
    for part in ref[2:].split("/"):
        node = node[part]
    return node


def type_ok(value, name):
    if name == "object":
        return isinstance(value, dict)
    if name == "array":
        return isinstance(value, list)
    if name == "string":
        return isinstance(value, str)
    if name == "integer":
        return isinstance(value, int) and not isinstance(value, bool)
    if name == "number":
        return isinstance(value, (int, float)) and not isinstance(value, bool)
    if name == "boolean":
        return isinstance(value, bool)
    if name == "null":
        return value is None
    raise ValueError("unsupported type %s" % name)


def validate(value, node, path):
    if "$ref" in node:
        validate(value, resolve(node["$ref"]), path)
        return
    for branch in node.get("allOf", []):
        validate(value, branch, path)
    if "const" in node and value != node["const"]:
        fail("%s: expected %r" % (path, node["const"]))
    if "type" in node:
        names = node["type"] if isinstance(node["type"], list) else [node["type"]]
        if not any(type_ok(value, name) for name in names):
            fail("%s: expected type %s" % (path, "/".join(names)))
            return
    if "enum" in node and value not in node["enum"]:
        fail("%s: not one of %s" % (path, node["enum"]))
    if isinstance(value, str):
        if "minLength" in node and len(value) < node["minLength"]:
            fail("%s: shorter than %d" % (path, node["minLength"]))
        if "pattern" in node and not re.search(node["pattern"], value):
            fail("%s: does not match %s" % (path, node["pattern"]))
    if isinstance(value, int) and not isinstance(value, bool) and "minimum" in node and value < node["minimum"]:
        fail("%s: below %d" % (path, node["minimum"]))
    if isinstance(value, list):
        if "minItems" in node and len(value) < node["minItems"]:
            fail("%s: fewer than %d items" % (path, node["minItems"]))
        if "maxItems" in node and len(value) > node["maxItems"]:
            fail("%s: more than %d items" % (path, node["maxItems"]))
        if "items" in node:
            for index, item in enumerate(value):
                validate(item, node["items"], "%s[%d]" % (path, index))
    if isinstance(value, dict):
        properties = node.get("properties", {})
        for name in node.get("required", []):
            if name not in value:
                fail("%s: missing %s" % (path, name))
        for name, item in value.items():
            if name in properties:
                validate(item, properties[name], "%s.%s" % (path, name))
            elif node.get("additionalProperties") is False:
                fail("%s: unexpected property %s" % (path, name))


validate(manifest, schema, "manifest")

if not isinstance(manifest, dict):
    if not errors:
        fail("manifest: expected an object")
    for message in errors:
        print("error: %s" % message, file=sys.stderr)
    print("compat manifest invalid: %d problem(s)" % len(errors), file=sys.stderr)
    sys.exit(1)

rows = manifest.get("rows") if isinstance(manifest.get("rows"), list) else []
retained = manifest.get("retained_uncalled") if isinstance(manifest.get("retained_uncalled"), list) else []
fields = manifest.get("field_mappings") if isinstance(manifest.get("field_mappings"), list) else []
sorts = manifest.get("sort_mappings") if isinstance(manifest.get("sort_mappings"), list) else []
filters = manifest.get("filter_translations") if isinstance(manifest.get("filter_translations"), list) else []
operators = manifest.get("operators") if isinstance(manifest.get("operators"), list) else []
profiles = manifest.get("profiles") if isinstance(manifest.get("profiles"), dict) else {}
compat_profiles = profiles.get("navidrome_compatibility", [])
extensions = profiles.get("opensubsonic_extensions", [])

counts = manifest.get("counts") if isinstance(manifest.get("counts"), dict) else {}
resolved = {
    "subsonic": sum(1 for row in rows if isinstance(row, dict) and row.get("surface") == "subsonic"),
    "navidrome": sum(1 for row in rows if isinstance(row, dict) and row.get("surface") == "navidrome"),
    "support": sum(1 for row in rows if isinstance(row, dict) and row.get("surface") == "support"),
    "retained_uncalled": len(retained),
    "field_mappings": len(fields),
    "sort_mappings": len(sorts),
    "filter_translations": len(filters),
    "operators": len(operators),
    "navidrome_compatibility_profiles": len(compat_profiles),
    "opensubsonic_extensions": len(extensions),
}
if set(counts) != set(resolved):
    fail("counts keys %s do not match the manifest groups %s" % (sorted(counts), sorted(resolved)))
for key, length in resolved.items():
    if counts.get(key) != length:
        fail("counts.%s is %r but the array length is %d" % (key, counts.get(key), length))
    if key in PINNED_COUNTS and length != PINNED_COUNTS[key]:
        fail("%s must be %d, found %d" % (key, PINNED_COUNTS[key], length))

ids = [row.get("id") for row in rows + retained if isinstance(row, dict)]
if len(set(ids)) != len(ids):
    fail("row ids are not unique: %s" % sorted({i for i in ids if ids.count(i) > 1}))
pairs = [(row.get("surface"), row.get("name")) for row in rows + retained if isinstance(row, dict)]
if len(set(pairs)) != len(pairs):
    fail("duplicate (surface, name): %s" % sorted({p for p in pairs if pairs.count(p) > 1}))

row_ids = set(ids)


def check_status(value, path):
    if value == "conformant":
        fail("%s: conformant is not assignable in F04" % path)


def check_owner(value, path):
    if not isinstance(value, str) or not value:
        fail("%s: owner_task is missing" % path)
    elif value not in ledger:
        fail("%s: owner_task %s is not in the task ledger %s" % (path, value, ledger_path or "embedded set"))


def check_route_row(row, path, group):
    check_status(row.get("status"), path)
    check_owner(row.get("owner_task"), path)
    surface = row.get("surface")
    name = row.get("name") if isinstance(row.get("name"), str) else ""
    paths = row.get("paths") if isinstance(row.get("paths"), list) else []
    methods = row.get("methods") if isinstance(row.get("methods"), list) else []
    baseline = row.get("baseline")
    status = row.get("status")
    in_target = row.get("in_target")
    if not methods:
        fail("%s: methods must not be empty" % path)
    if group == "rows":
        if surface == "support" and in_target is not False:
            fail("%s: support row must have in_target false" % path)
        if surface in ("subsonic", "navidrome") and in_target is not True:
            fail("%s: target row must have in_target true" % path)
    else:
        if surface not in ("subsonic", "navidrome"):
            fail("%s: retained route must keep its subsonic or navidrome surface" % path)
        if in_target is not False:
            fail("%s: retained route must have in_target false" % path)
        if baseline != "present-uncalled":
            fail("%s: retained route must be present-uncalled" % path)
        if status != "registered":
            fail("%s: retained route must be registered" % path)
    if surface == "subsonic":
        expected = {"/rest/%s" % name, "/rest/%s.view" % name}
        if set(paths) != expected:
            fail("%s: subsonic paths must be %s" % (path, sorted(expected)))
    if surface == "navidrome":
        parts = name.split(" ", 1)
        if len(parts) != 2 or methods != [parts[0]] or paths != [parts[1]]:
            fail("%s: navidrome name, methods and paths disagree" % path)
    if surface in ("subsonic", "navidrome"):
        if baseline == "present" and status != "registered":
            fail("%s: present path must be registered" % path)
        if baseline == "missing" and status != "unproved":
            fail("%s: missing path must be unproved" % path)
        if baseline == "present-uncalled" and status != "registered":
            fail("%s: present-uncalled path must be registered" % path)


for index, row in enumerate(rows):
    if isinstance(row, dict):
        check_route_row(row, "rows[%d]" % index, "rows")
    else:
        fail("rows[%d]: not an object" % index)

for index, row in enumerate(retained):
    if isinstance(row, dict):
        check_route_row(row, "retained_uncalled[%d]" % index, "retained")
    else:
        fail("retained_uncalled[%d]: not an object" % index)

for group, entries in (
    ("field_mappings", fields),
    ("sort_mappings", sorts),
    ("filter_translations", filters),
    ("operators", operators),
):
    for index, entry in enumerate(entries):
        path = "%s[%d]" % (group, index)
        if isinstance(entry, dict):
            check_status(entry.get("status"), path)
            check_owner(entry.get("owner_task"), path)
            check_owner(entry.get("facade_owner"), path)
        else:
            fail("%s: not an object" % path)

for index, entry in enumerate(compat_profiles):
    path = "profiles.navidrome_compatibility[%d]" % index
    if isinstance(entry, dict):
        check_status(entry.get("status"), path)
        owners = entry.get("owner_tasks", [])
        if not owners:
            fail("%s: owner_tasks is empty" % path)
        for owner in owners:
            check_owner(owner, path)
        for endpoint in entry.get("endpoint_ids", []):
            if endpoint not in row_ids:
                fail("%s: unknown endpoint id %s" % (path, endpoint))

for index, entry in enumerate(extensions):
    path = "profiles.opensubsonic_extensions[%d]" % index
    if isinstance(entry, dict):
        check_status(entry.get("status"), path)
        owners = entry.get("owner_tasks", [])
        if not owners:
            fail("%s: owner_tasks is empty" % path)
        for owner in owners:
            check_owner(owner, path)
        for endpoint in entry.get("endpoint_ids", []):
            if endpoint not in row_ids:
                fail("%s: unknown endpoint id %s" % (path, endpoint))

expected_thresholds = ["baseline", "0.48.0", "0.49.3", "0.55.0", "0.56.0", "0.57.0", "0.60.4", "0.61.0"]
found_thresholds = [entry.get("threshold") for entry in compat_profiles if isinstance(entry, dict)]
if found_thresholds != expected_thresholds:
    fail("navidrome compatibility thresholds must be %s, found %s" % (expected_thresholds, found_thresholds))

expected_extensions = ["formPost", "indexBasedQueue", "playbackReport", "songLyrics", "topSongsByArtistId", "transcoding"]
found_extensions = [entry.get("name") for entry in extensions if isinstance(entry, dict)]
if sorted(found_extensions) != sorted(expected_extensions):
    fail("opensubsonic extensions must be %s, found %s" % (sorted(expected_extensions), sorted(found_extensions)))

if manifest.get("status_values") != ["unproved", "registered", "conformant"]:
    fail("status_values must be the closed status enum")


def walk_keys(value, path):
    if isinstance(value, dict):
        for key, item in value.items():
            if FORBIDDEN_KEYS.match(key):
                fail("%s.%s: coverage claims may not carry a completion flag" % (path, key))
            walk_keys(item, "%s.%s" % (path, key))
    elif isinstance(value, list):
        for index, item in enumerate(value):
            walk_keys(item, "%s[%d]" % (path, index))


walk_keys(manifest, "manifest")

if errors:
    limit = 25
    for message in errors[:limit]:
        print("error: %s" % message, file=sys.stderr)
    if len(errors) > limit:
        print("error: %d further problem(s) suppressed" % (len(errors) - limit), file=sys.stderr)
    print("compat manifest invalid: %d problem(s)" % len(errors), file=sys.stderr)
    sys.exit(1)

for key in resolved:
    print("%s %d" % (key, resolved[key]))
print("compat manifest valid")
PY
