#!/usr/bin/env bash
# Fresh-install smoke test: install a wheel into a new virtual environment,
# run a flow offline, and list it. Usage: scripts/smoke.sh <wheel-or-package-spec>
set -euo pipefail
SPEC="${1:-cereyan}"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
python3 -m venv "$TMP/venv"
if [ -x "$TMP/venv/bin/python" ]; then PY="$TMP/venv/bin/python"; else PY="$TMP/venv/Scripts/python.exe"; fi
"$PY" -m pip install --quiet --upgrade pip
"$PY" -m pip install --quiet "$SPEC"
"$PY" -m pip show cereyan >/dev/null
"$PY" -c "import cereyan; print('cereyan', cereyan.__version__)"
cat > "$TMP/pipeline.py" <<'PY'
from cereyan import flow, task

@task
def add(a, b):
    return a + b

@flow
def smoke():
    return add(1, 2)

if __name__ == "__main__":
    assert smoke() == 3
    print("offline run ok")
PY
export CEREYAN_HOME="$TMP/home"
"$PY" "$TMP/pipeline.py"
"$PY" -m cereyan runs ls --json | "$PY" -c "import json,sys; runs=json.load(sys.stdin); assert runs and runs[0]['state']['type']=='Completed', runs; print('runs ls ok')"
# A served API with a token answers health openly and everything else only with the token.
export CEREYAN_TOKEN="smoke-token"
export CEREYAN_NO_BROWSER=1
"$PY" -m cereyan serve "$TMP" --port 0 --no-open >"$TMP/serve.log" 2>&1 &
SERVE_PID=$!
for _ in $(seq 1 100); do [ -f "$CEREYAN_HOME/server.json" ] && break; sleep 0.2; done
"$PY" - <<'PY'
import json, os, urllib.request, urllib.error
info = json.load(open(os.path.join(os.environ["CEREYAN_HOME"], "server.json")))
assert info["auth"] is True, info
url = info["url"]
assert urllib.request.urlopen(url + "/api/health", timeout=5).status == 200
try:
    urllib.request.urlopen(url + "/api/runs", timeout=5)
    raise SystemExit("expected 401 without a token")
except urllib.error.HTTPError as exc:
    assert exc.code == 401, exc.code
req = urllib.request.Request(url + "/api/runs", headers={"authorization": "Bearer smoke-token"})
assert urllib.request.urlopen(req, timeout=5).status == 200
print("token check ok")
PY
kill "$SERVE_PID" 2>/dev/null || true
wait "$SERVE_PID" 2>/dev/null || true
echo "smoke test passed"
