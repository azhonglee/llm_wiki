#!/usr/bin/env bash
# Validate deployment inputs without starting the Web server.

set -euo pipefail

if [[ $# -gt 1 || ( $# -eq 1 && "$1" != "--quiet" ) ]]; then
  printf '用法: %s [--quiet]\n' "$0" >&2
  exit 2
fi

script_dir="$(cd -- "$(dirname -- "$0")" && pwd)"
# shellcheck source=lib-web-env.sh
source "$script_dir/lib-web-env.sh"

web_load_config
web_validate_environment

if [[ "${1:-}" != "--quiet" ]]; then
  printf 'Web 部署环境检查通过：host=%s port=%s workspace=%s data=%s\n' \
    "$LLM_WIKI_WEB_HOST" "$LLM_WIKI_WEB_PORT" \
    "$LLM_WIKI_WEB_WORKSPACE_ROOT" "$LLM_WIKI_WEB_DATA_DIR"
fi
