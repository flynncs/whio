default:
    @just --list

setup profile="personal":
    python3 tools/dev.py setup {{ quote(profile) }}

start profile="personal":
    python3 tools/dev.py start {{ quote(profile) }}

full profile="personal":
    python3 tools/dev.py start {{ quote(profile) }} --full

local profile="personal":
    python3 tools/dev.py start {{ quote(profile) }} --local

stop profile="personal":
    python3 tools/dev.py stop {{ quote(profile) }}

status profile="personal":
    python3 tools/dev.py status {{ quote(profile) }}

containerize profile="personal":
    python3 tools/dev.py containerize {{ quote(profile) }}

resources profile="personal":
    python3 tools/dev.py resources {{ quote(profile) }}

monitor profile="personal":
    python3 tools/dev.py monitor {{ quote(profile) }}

services profile="personal":
    python3 tools/dev.py services {{ quote(profile) }}

logs profile="personal" service="api":
    python3 tools/dev.py logs {{ quote(profile) }} --service {{ quote(service) }}

reload profile="dev":
    python3 tools/dev.py reload {{ quote(profile) }}

watch profile="dev":
    python3 tools/dev.py watch {{ quote(profile) }}

migrations profile="dev":
    python3 tools/dev.py migrations {{ quote(profile) }}

migrate profile="dev":
    python3 tools/dev.py migrate {{ quote(profile) }}

upgrade profile="personal":
    python3 tools/dev.py upgrade {{ quote(profile) }}

backup profile="personal":
    python3 tools/dev.py backup {{ quote(profile) }}

backup-check profile="personal":
    python3 tools/dev.py backup-check {{ quote(profile) }}

login profile="personal":
    python3 tools/dev.py login {{ quote(profile) }}

media-import *args:
    python3 tools/dev.py media-import {{ args }}

media-verify:
    python3 tools/dev.py media-verify

cleanup:
    python3 tools/dev.py cleanup

test filter:
    python3 tools/test-core.py --docker -- --lib {{ quote(filter) }}

test-db filter:
    python3 tools/test-postgres.py --docker -- --lib {{ quote(filter) }}

test-target target:
    python3 tools/test-postgres.py --docker -- --test {{ quote(target) }}

agent-create name packet base="HEAD" source="head":
    python3 tools/agent_worktree.py create {{ quote(name) }} --packet {{ quote(packet) }} --base {{ quote(base) }} --source {{ quote(source) }}

agent-status name:
    python3 tools/agent_worktree.py status {{ quote(name) }}

agent-retire name:
    python3 tools/agent_worktree.py retire {{ quote(name) }}
