#!/usr/bin/env bash
# End-to-end smoke test for the headless Web server and its browser API contract.

set -euo pipefail

repo_root="$(cd -- "$(dirname -- "$0")/.." && pwd)"
mkdir -p "$HOME/tmp"
work_dir="$(mktemp -d "$HOME/tmp/llm-wiki-web-e2e.XXXXXX")"
server_pid=""
provider_pid=""

cleanup() {
  if [[ -n "$server_pid" ]]; then kill "$server_pid" 2>/dev/null || true; fi
  if [[ -n "$provider_pid" ]]; then kill "$provider_pid" 2>/dev/null || true; fi
  wait "$server_pid" 2>/dev/null || true
  wait "$provider_pid" 2>/dev/null || true
  python3 - "$work_dir" <<'PY'
import shutil
import sys
shutil.rmtree(sys.argv[1], ignore_errors=True)
PY
}
trap cleanup EXIT INT TERM

free_port() {
  python3 - <<'PY'
import socket
with socket.socket() as sock:
    sock.bind(("127.0.0.1", 0))
    print(sock.getsockname()[1])
PY
}

server_port="$(free_port)"
provider_port="$(free_port)"
base="http://127.0.0.1:${server_port}/api/v2"
origin="http://127.0.0.1:${server_port}"
token="web-e2e-token-0123456789abcdef0123456789"
cookie_jar="$work_dir/cookies.txt"

mkdir -p "$work_dir/workspace" "$work_dir/data"

cat > "$work_dir/provider.py" <<'PY'
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import sys

class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get("content-length", "0"))
        payload = json.loads(self.rfile.read(length) or b"{}")
        messages = payload.get("messages", [])
        text = messages[-1].get("content", "") if messages else ""
        body = json.dumps({"choices": [{"message": {"content": "echo: " + text}}]}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):
        pass

HTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()
PY
python3 "$work_dir/provider.py" "$provider_port" >"$work_dir/provider.log" 2>&1 &
provider_pid=$!

if [[ -n "${LLM_WIKI_WEB_SERVER_BIN:-}" ]]; then
  server_bin="$LLM_WIKI_WEB_SERVER_BIN"
else
  cargo build --offline --manifest-path "$repo_root/src-tauri/server/Cargo.toml" --bin llm-wiki-server
  server_bin="$repo_root/src-tauri/server/target/debug/llm-wiki-server"
fi
if [[ ! -f "$repo_root/dist/index.html" ]]; then
  (cd "$repo_root" && npm run build)
fi

env -u DISPLAY -u WAYLAND_DISPLAY \
  LLM_WIKI_BOOTSTRAP_TOKEN="$token" \
  "$server_bin" \
  --host 127.0.0.1 \
  --port "$server_port" \
  --workspace-root "$work_dir/workspace" \
  --data-dir "$work_dir/data" \
  --web-root "$repo_root/dist" \
  >"$work_dir/server.log" 2>&1 &
server_pid=$!

for _ in $(seq 1 100); do
  if curl --silent --fail "$base/health/live" >/dev/null 2>&1; then break; fi
  sleep 0.05
done
if ! curl --silent --fail "$base/health/live" >/dev/null; then
  cat "$work_dir/server.log" >&2
  printf 'Web server did not become ready\n' >&2
  exit 1
fi

json_field() {
  python3 -c 'import json,sys; print(json.load(sys.stdin)[sys.argv[1]])' "$1"
}

request_status() {
  curl --silent --show-error --output "$work_dir/response.body" --write-out '%{http_code}' "$@"
}

assert_status() {
  local expected="$1" actual="$2" label="$3"
  if [[ "$actual" != "$expected" ]]; then
    printf 'FAIL %s: expected %s, got %s\n' "$label" "$expected" "$actual" >&2
    cat "$work_dir/response.body" >&2 || true
    exit 1
  fi
  printf 'PASS %s (%s)\n' "$label" "$actual"
}

assert_status 200 "$(request_status "http://127.0.0.1:${server_port}/")" static_ui
assert_status 401 "$(request_status "$base/projects")" unauthenticated_guard

login="$(curl --silent --show-error --cookie-jar "$cookie_jar" \
  -H "Origin: $origin" -H 'Content-Type: application/json' \
  --data "{\"token\":\"$token\"}" "$base/auth/login")"
csrf="$(printf '%s' "$login" | json_field csrfToken)"
[[ -n "$csrf" ]] || { printf 'Login did not return CSRF token\n' >&2; exit 1; }
auth=(-b "$cookie_jar" -H "Origin: $origin" -H "X-CSRF-Token: $csrf")

assert_status 200 "$(request_status -b "$cookie_jar" "$base/auth/session")" session
assert_status 200 "$(request_status -b "$cookie_jar" "$base/auth/csrf")" csrf_refresh_get

# Refreshing CSRF rotates it, so log in again to use the token returned by login.
login="$(curl --silent --show-error --cookie-jar "$cookie_jar" \
  -H "Origin: $origin" -H 'Content-Type: application/json' \
  --data "{\"token\":\"$token\"}" "$base/auth/login")"
csrf="$(printf '%s' "$login" | json_field csrfToken)"
auth=(-b "$cookie_jar" -H "Origin: $origin" -H "X-CSRF-Token: $csrf")

project_json="$(curl --silent --show-error "${auth[@]}" -H 'Content-Type: application/json' \
  --data '{"name":"demo"}' "$base/projects")"
project_id="$(printf '%s' "$project_json" | json_field id)"
assert_status 200 "$(request_status -b "$cookie_jar" "$base/projects/$project_id")" project_detail
renamed="$(curl --silent --show-error "${auth[@]}" -H 'Content-Type: application/json' -X PATCH \
  --data '{"name":"renamed demo"}' "$base/projects/$project_id")"
printf '%s' "$renamed" | python3 -c 'import json,sys; assert json.load(sys.stdin)["name"] == "renamed demo"'
printf 'PASS project_rename\n'

tree="$(curl --silent --show-error -b "$cookie_jar" "$base/projects/$project_id/tree")"
printf '%s' "$tree" | python3 -c 'import json,sys; d=json.load(sys.stdin); assert any(n["name"] == "wiki" and n["isDir"] for n in d["tree"])'
printf 'PASS recursive_tree\n'

content="$(curl --silent --show-error -b "$cookie_jar" "$base/projects/$project_id/files/content?path=README.md")"
revision="$(printf '%s' "$content" | json_field revision)"
saved="$(curl --silent --show-error "${auth[@]}" -H "If-Match: $revision" \
  -H 'Content-Type: application/json' -X PUT \
  --data "{\"content\":\"# Updated\\n\\n[[Second]]\\n\",\"revision\":\"$revision\"}" \
  "$base/projects/$project_id/files/content?path=README.md")"
printf '%s' "$saved" | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d["content"].startswith("# Updated") and len(d["revision"]) == 64'
printf 'PASS revision_write\n'
assert_status 409 "$(request_status "${auth[@]}" -H "If-Match: $revision" \
  -H 'Content-Type: application/json' -X PUT \
  --data "{\"content\":\"stale\",\"revision\":\"$revision\"}" \
  "$base/projects/$project_id/files/content?path=README.md")" stale_write_conflict

created="$(curl --silent --show-error "${auth[@]}" -H 'If-Match: *' \
  -H 'Content-Type: application/json' -X PUT \
  --data '{"content":"# New page\n","revision":"*"}' \
  "$base/projects/$project_id/files/content?path=wiki/new-page.md")"
printf '%s' "$created" | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d["path"] == "wiki/new-page.md"'
printf 'PASS create_text\n'
assert_status 200 "$(request_status "${auth[@]}" -H 'Content-Type: application/json' \
  --data '{"sourcePath":"wiki/new-page.md","targetPath":"wiki/moved-page.md"}' \
  "$base/projects/$project_id/files/move")" move_file
assert_status 200 "$(request_status -b "$cookie_jar" "$base/projects/$project_id/files/content?path=wiki/moved-page.md")" moved_file_read

assert_status 201 "$(request_status "${auth[@]}" -H 'Content-Type: application/json' \
  --data '{"path":"raw/sources/uploads"}' "$base/projects/$project_id/directories")" create_directory
printf 'hello upload\n' > "$work_dir/source.txt"
assert_status 201 "$(request_status "${auth[@]}" -F "files=@$work_dir/source.txt" \
  "$base/projects/$project_id/uploads?path=raw/sources/uploads")" multipart_upload
asset="$base/projects/$project_id/assets?path=raw/sources/uploads/source.txt"
assert_status 200 "$(request_status -b "$cookie_jar" "$asset")" asset_get
assert_status 200 "$(request_status -I -b "$cookie_jar" "$asset")" asset_head

search="$(curl --silent --show-error "${auth[@]}" -H 'Content-Type: application/json' \
  --data '{"query":"Updated"}' "$base/projects/$project_id/search")"
printf '%s' "$search" | python3 -c 'import json,sys; d=json.load(sys.stdin); assert any(r["path"] == "README.md" for r in d["results"])'
printf 'PASS search\n'
assert_status 200 "$(request_status -b "$cookie_jar" "$base/projects/$project_id/graph")" graph
assert_status 200 "$(request_status -b "$cookie_jar" "$base/projects/$project_id/reviews")" reviews

assert_status 200 "$(request_status "${auth[@]}" -H 'Content-Type: application/json' -X PATCH \
  --data "{\"ui\":{\"language\":\"zh\"},\"webChat\":{\"endpoint\":\"http://127.0.0.1:${provider_port}/v1/chat/completions\",\"model\":\"test-model\",\"apiKey\":\"must-not-return\"}}" "$base/settings")" settings_patch
settings="$(curl --silent --show-error -b "$cookie_jar" "$base/settings")"
printf '%s' "$settings" | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d["ui"]["language"] == "zh" and d["webChat"]["apiKey"]["configured"] is True'
printf 'PASS settings_redaction\n'

chat="$(curl --silent --show-error "${auth[@]}" -H 'Content-Type: application/json' \
  --data '{"title":"test"}' "$base/projects/$project_id/chat/sessions")"
chat_id="$(printf '%s' "$chat" | json_field id)"
chat_events="$(curl --silent --show-error --max-time 5 "${auth[@]}" \
  -H 'Accept: text/event-stream' -H 'Content-Type: application/json' \
  --data '{"message":"hello"}' "$base/projects/$project_id/chat/sessions/$chat_id/turns")"
printf '%s' "$chat_events" | grep -q 'echo: hello'
printf 'PASS chat_sse\n'

job="$(curl --silent --show-error "${auth[@]}" -X POST "$base/projects/$project_id/index/rebuild")"
job_id="$(printf '%s' "$job" | json_field id)"
printf '%s' "$job" | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d["type"] == "index-rebuild" and isinstance(d["progress"], dict)'
printf 'PASS job_shape\n'
assert_status 200 "$(request_status -b "$cookie_jar" "$base/jobs?projectId=$project_id")" jobs_filter
job_events="$(curl --silent --max-time 1 -b "$cookie_jar" "$base/jobs/$job_id/events" || true)"
printf '%s' "$job_events" | grep -q 'event: job'
printf 'PASS job_sse\n'

job_status="queued"
for _ in $(seq 1 50); do
  job_status="$(curl --silent --show-error -b "$cookie_jar" "$base/jobs/$job_id" | json_field status)"
  [[ "$job_status" == "completed" ]] && break
  sleep 0.05
done
[[ "$job_status" == "completed" ]] || { printf 'Index job did not complete\n' >&2; exit 1; }
printf 'PASS index_job\n'
index_content="$(curl --silent --show-error -b "$cookie_jar" "$base/projects/$project_id/files/content?path=wiki/index.md")"
printf '%s' "$index_content" | python3 -c 'import json,sys; assert "[[moved-page|New page]]" in json.load(sys.stdin)["content"]'
printf 'PASS index_content\n'

retry="$(curl --silent --show-error "${auth[@]}" -X POST "$base/jobs/$job_id/retry")"
retry_id="$(printf '%s' "$retry" | json_field id)"
retry_status="queued"
for _ in $(seq 1 50); do
  retry_status="$(curl --silent --show-error -b "$cookie_jar" "$base/jobs/$retry_id" | json_field status)"
  [[ "$retry_status" == "completed" ]] && break
  sleep 0.05
done
[[ "$retry_status" == "completed" ]] || { printf 'Retried index job did not complete\n' >&2; exit 1; }
printf 'PASS job_retry\n'

assert_status 400 "$(request_status -b "$cookie_jar" "$base/projects/$project_id/files/content?path=../../etc/passwd")" path_traversal

assert_status 204 "$(request_status "${auth[@]}" -X DELETE "$base/projects/$project_id")" project_unregister
[[ -d "$work_dir/workspace/demo" ]] || { printf 'Unregister removed project files\n' >&2; exit 1; }
assert_status 404 "$(request_status -b "$cookie_jar" "$base/projects/$project_id")" unregistered_project_hidden
printf 'All Web HTTP smoke checks passed.\n'
