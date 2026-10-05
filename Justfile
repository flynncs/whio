default:
    @python3 tools/dev.py status

status profile="personal":
    python3 tools/dev.py status {{ quote(profile) }}

up profile="personal":
    python3 tools/dev.py up {{ quote(profile) }}

down profile="personal":
    python3 tools/dev.py down {{ quote(profile) }}

logs profile="personal" service="":
    python3 tools/dev.py logs {{ quote(profile) }}{{ if service != "" { " --service " + quote(service) } else { "" } }}

deploy:
    python3 tools/dev.py deploy personal

dev profile="dev":
    python3 tools/dev.py dev {{ quote(profile) }}

migrations profile="dev":
    python3 tools/dev.py migrations {{ quote(profile) }}

backup profile="personal":
    python3 tools/dev.py backup {{ quote(profile) }}

test +filters:
    python3 tools/test-core.py --docker -- --lib {{ filters }}

test-db +filters:
    python3 tools/test-postgres.py --docker -- --lib {{ filters }}

test-it +filters:
    python3 tools/test-postgres.py --docker -- --test integration {{ filters }}

media action *args:
    {{ if action == "import" { "python3 tools/dev.py media-import" } else if action == "verify" { "python3 tools/dev.py media-verify" } else { error("media takes import or verify") } }} {{ args }}

clean:
    python3 tools/dev.py clean
