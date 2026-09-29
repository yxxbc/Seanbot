#!/usr/bin/env bash
# 输出工作区版本号（根 Cargo.toml 的 [workspace.package] version）。
set -euo pipefail
file="${1:-Cargo.toml}"
awk '
  /^\[/ { in_pkg = ($0 == "[workspace.package]") }
  in_pkg && /^version[[:space:]]*=/ {
    gsub(/^version[[:space:]]*=[[:space:]]*"|".*$/, "")
    print
    exit
  }
' "$file"
