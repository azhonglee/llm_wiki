#!/usr/bin/env bash
# Shared helpers for Web Edition deployment scripts. This file is sourced.

set -euo pipefail

web_script_dir() {
  cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd
}

web_repo_root() {
  cd -- "$(web_script_dir)/.." && pwd
}

web_is_ipv4() {
  local value="$1" octet
  [[ "$value" =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}$ ]] || return 1
  IFS=. read -r -a octets <<< "$value"
  for octet in "${octets[@]}"; do
    (( 10#$octet <= 255 )) || return 1
  done
}

web_detect_lan_ipv4() {
  local address
  address="$(ip -4 route get 1.1.1.1 2>/dev/null | sed -n 's/.* src \([0-9.]*\).*/\1/p' | head -n 1)"
  if web_is_ipv4 "$address" && [[ "$address" != 127.* ]]; then
    printf '%s\n' "$address"
    return 0
  fi
  hostname -I 2>/dev/null | tr ' ' '\n' | while IFS= read -r address; do
    if web_is_ipv4 "$address" && [[ "$address" != 127.* && "$address" != 172.17.* ]]; then
      printf '%s\n' "$address"
      break
    fi
  done
}

web_validate_config_file() {
  local config_file="$1" mode owner

  if [[ ! -f "$config_file" || -L "$config_file" ]]; then
    printf '错误: Web 配置必须是普通文件且不能是符号链接: %s\n' "$config_file" >&2
    return 1
  fi

  mode="$(stat -c '%a' "$config_file")"
  owner="$(stat -c '%u' "$config_file")"
  if (( (8#$mode & 077) != 0 )); then
    printf '错误: Web 配置不能授予 group/other 任何权限，请执行 chmod 600 %s（当前 %s）。\n' \
      "$config_file" "$mode" >&2
    return 1
  fi
  if [[ "$owner" != "$(id -u)" ]]; then
    printf '错误: Web 配置必须由当前服务用户拥有: %s\n' "$config_file" >&2
    return 1
  fi
}

web_load_config() {
  local repo_root config_file
  repo_root="$(web_repo_root)"
  config_file="${LLM_WIKI_WEB_CONFIG:-}"

  if [[ -n "$config_file" ]]; then
    if [[ ! -r "$config_file" ]]; then
      printf '错误: 无法读取 LLM_WIKI_WEB_CONFIG=%s\n' "$config_file" >&2
      return 1
    fi
    web_validate_config_file "$config_file"
    # The configuration file is operator-controlled and must be mode 0600.
    # shellcheck source=/dev/null
    set -a
    source "$config_file"
    set +a
  elif [[ -z "${LLM_WIKI_WEB_SERVER_BIN:-}" && -r "$repo_root/.env.web.local" ]]; then
    web_validate_config_file "$repo_root/.env.web.local"
    # shellcheck source=/dev/null
    set -a
    source "$repo_root/.env.web.local"
    set +a
    config_file="$repo_root/.env.web.local"
  fi

  WEB_CONFIG_FILE="$config_file"
  if [[ -z "${LLM_WIKI_WEB_PUBLIC_IPV4:-}" ]]; then
    LLM_WIKI_WEB_PUBLIC_IPV4="$(web_detect_lan_ipv4 || true)"
  fi
  LLM_WIKI_WEB_PUBLIC_HTTPS_PORT="${LLM_WIKI_WEB_PUBLIC_HTTPS_PORT:-8443}"
  export LLM_WIKI_WEB_PUBLIC_IPV4 LLM_WIKI_WEB_PUBLIC_HTTPS_PORT
  export WEB_CONFIG_FILE
}

web_is_loopback_host() {
  case "$1" in
    127.*|::1|\[::1\]) return 0 ;;
    *) return 1 ;;
  esac
}

web_validate_environment() {
  local host port workspace data_dir web_root bin token public_ipv4 public_port path error_count=0

  bin="${LLM_WIKI_WEB_SERVER_BIN:-}"
  host="${LLM_WIKI_WEB_HOST:-}"
  port="${LLM_WIKI_WEB_PORT:-}"
  workspace="${LLM_WIKI_WEB_WORKSPACE_ROOT:-}"
  data_dir="${LLM_WIKI_WEB_DATA_DIR:-}"
  web_root="${LLM_WIKI_WEB_ROOT:-}"
  token="${LLM_WIKI_WEB_BOOTSTRAP_TOKEN:-}"
  public_ipv4="${LLM_WIKI_WEB_PUBLIC_IPV4:-}"
  public_port="${LLM_WIKI_WEB_PUBLIC_HTTPS_PORT:-8443}"

  for path in LLM_WIKI_WEB_SERVER_BIN LLM_WIKI_WEB_HOST LLM_WIKI_WEB_PORT \
    LLM_WIKI_WEB_WORKSPACE_ROOT LLM_WIKI_WEB_DATA_DIR LLM_WIKI_WEB_ROOT \
    LLM_WIKI_WEB_BOOTSTRAP_TOKEN; do
    if [[ -z "${!path:-}" ]]; then
      printf '错误: 缺少 %s\n' "$path" >&2
      error_count=$((error_count + 1))
    fi
  done

  if [[ -n "$bin" && ! -x "$bin" ]]; then
    printf '错误: 服务二进制不存在或不可执行: %s\n' "$bin" >&2
    error_count=$((error_count + 1))
  fi

  if [[ -n "$port" ]] && { [[ ! "$port" =~ ^[0-9]+$ ]] || (( port < 1 || port > 65535 )); }; then
    printf '错误: LLM_WIKI_WEB_PORT 必须是 1 到 65535 的整数: %s\n' "$port" >&2
    error_count=$((error_count + 1))
  fi

  if [[ -n "$public_ipv4" ]] && ! web_is_ipv4 "$public_ipv4"; then
    printf '错误: LLM_WIKI_WEB_PUBLIC_IPV4 不是有效 IPv4: %s\n' "$public_ipv4" >&2
    error_count=$((error_count + 1))
  fi
  if [[ ! "$public_port" =~ ^[0-9]+$ ]] || (( public_port < 1 || public_port > 65535 )); then
    printf '错误: LLM_WIKI_WEB_PUBLIC_HTTPS_PORT 必须是 1 到 65535 的整数: %s\n' "$public_port" >&2
    error_count=$((error_count + 1))
  fi

  for path in "$workspace" "$data_dir"; do
    if [[ -n "$path" && "$path" != /* ]]; then
      printf '错误: 路径必须为绝对路径: %s\n' "$path" >&2
      error_count=$((error_count + 1))
    elif [[ -n "$path" && ! -d "$path" ]]; then
      printf '错误: 目录不存在: %s\n' "$path" >&2
      error_count=$((error_count + 1))
    elif [[ -n "$path" && ! -w "$path" ]]; then
      printf '错误: 当前服务用户不可写目录: %s\n' "$path" >&2
      error_count=$((error_count + 1))
    fi
  done

  if [[ -n "$web_root" && "$web_root" != /* ]]; then
    printf '错误: LLM_WIKI_WEB_ROOT 必须为绝对路径: %s\n' "$web_root" >&2
    error_count=$((error_count + 1))
  elif [[ -n "$web_root" && ! -r "$web_root/index.html" ]]; then
    printf '错误: Web 静态资源目录中缺少可读的 index.html: %s\n' "$web_root" >&2
    error_count=$((error_count + 1))
  fi

  if [[ -n "$token" && ${#token} -lt 32 ]]; then
    printf '错误: LLM_WIKI_WEB_BOOTSTRAP_TOKEN 至少需要 32 个字符，请使用随机值。\n' >&2
    error_count=$((error_count + 1))
  fi

  # The current server has no native TLS listener. Never pass its
  # --allow-insecure-remote escape hatch from a deployment script.
  if [[ -n "$host" ]] && ! web_is_loopback_host "$host"; then
    printf '错误: 当前 Web 服务仅允许 loopback 监听；请使用 127.0.0.1 + Caddy/Nginx HTTPS，不能直接暴露 %s。\n' "$host" >&2
    error_count=$((error_count + 1))
  fi

  return "$error_count"
}
