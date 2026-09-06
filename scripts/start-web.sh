#!/usr/bin/env bash
# Start the headless Web Edition server with validated deployment settings.

set -euo pipefail

if [[ $# -ne 0 ]]; then
  printf '错误: 为防止绕过网络与认证校验，start-web.sh 不接受额外参数；请修改配置文件。\n' >&2
  exit 2
fi

script_dir="$(cd -- "$(dirname -- "$0")" && pwd)"
# shellcheck source=lib-web-env.sh
source "$script_dir/lib-web-env.sh"

web_load_config
web_validate_environment

args=(
  --host "$LLM_WIKI_WEB_HOST"
  --port "$LLM_WIKI_WEB_PORT"
  --workspace-root "$LLM_WIKI_WEB_WORKSPACE_ROOT"
  --data-dir "$LLM_WIKI_WEB_DATA_DIR"
  --web-root "$LLM_WIKI_WEB_ROOT"
  --secure-cookie
)

# Do not put the bootstrap token on the command line, where it can leak through
# process listings. The server accepts this environment variable directly.
umask 077
export LLM_WIKI_BOOTSTRAP_TOKEN="$LLM_WIKI_WEB_BOOTSTRAP_TOKEN"
if [[ -n "${LLM_WIKI_WEB_PUBLIC_IPV4:-}" ]]; then
  printf 'LLM Wiki 局域网 HTTPS 地址：https://%s:%s\n' \
    "$LLM_WIKI_WEB_PUBLIC_IPV4" "$LLM_WIKI_WEB_PUBLIC_HTTPS_PORT" >&2
fi
exec "$LLM_WIKI_WEB_SERVER_BIN" "${args[@]}"
