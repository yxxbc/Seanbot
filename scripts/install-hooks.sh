#!/usr/bin/env bash
#
# 安装项目 git hooks（每个克隆只需执行一次）
#
#   bash scripts/install-hooks.sh
#
# 安装后的效果：
#   commit-msg —— 校验提交信息是否符合 Conventional Commits（不合格直接拒绝）
#   pre-commit —— 拒绝大文件、疑似密钥文件、含冲突标记的文件
#
# 卸载：git config --unset core.hooksPath
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

hooks_dir=".githooks"

if [ ! -d "$hooks_dir" ]; then
  echo "✗ 未找到 $hooks_dir 目录（请在仓库根目录运行本脚本）" >&2
  exit 1
fi

chmod +x "$hooks_dir"/*

git config core.hooksPath "$hooks_dir"

echo "✓ git hooks 已安装（core.hooksPath = ${hooks_dir}）"
echo "  已启用："
for hook in "$hooks_dir"/*; do
  echo "    - $(basename "$hook")"
done
