#!/bin/sh
# 常驻 bash 会话的清理验证：退出程序后绝不允许留下孤儿进程。
#
# 覆盖的场景
#   1) drop     ：正常返回 → BashSessions::drop 杀进程组
#   2) exit     ：std::process::exit 跳过 Drop → 子 shell 靠 stdin 管道 EOF 自杀（兜底层）
#   3) closeall ：显式 close_all → 立刻收干净
#   4) timeout  ：命令超时 → 会话被关闭（由 cargo test 断言，这里复核不留进程）
#   5) 全局扫描 ：用随环境变量传播的标记扫 ps，任何残留都算失败
#
# 每个会话里都留一个 sleep 300 后台任务：只杀 shell、不杀进程组是过不了这个测试的。
#
# 用法：bash scripts/tests/bash_session_test.sh
set -u

MARKER="seanbot-bash-session-test-$$"
# 探针会把它继承给每个会话 shell，再由 shell 传给它的子进程
export SEANBOT_BASH_SESSION_MARKER="$MARKER"

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
cd "$ROOT" || exit 1

fail=0
note() { printf '%s\n' "$*"; }

# 按标记扫描残留进程（ps 是否显示环境依赖平台，扫不到时还有 pid 核对）。
# 扫描自身（本脚本与它拉起的 ps / grep / awk 管道）也带着标记环境，CI 上会被立刻看到，
# 所以按「工具名 + 脚本路径 + 自身 pid」把它们排除掉。
scan_marker() {
  ps eww -ax 2>/dev/null \
    | grep -F "$MARKER" \
    | grep -vE '(^|[[:space:]])(ps|grep|awk)([[:space:]]|$)' \
    | grep -vF "$0" \
    | grep -vE "^[[:space:]]*$$[[:space:]]" \
    || true
}

alive() { kill -0 "$1" 2>/dev/null; }

check_pids_gone() { # check_pids_gone <场景> <pid...>
  scenario=$1
  shift
  sleep 1
  for pid in "$@"; do
    [ "$pid" = "?" ] && continue
    if alive "$pid"; then
      note "✗ ${scenario}：进程 $pid 仍然活着"
      ps -o pid,ppid,stat,command -p "$pid" 2>/dev/null || true
      fail=1
    fi
  done
}

check_no_leftover() { # check_no_leftover <场景>
  leftover=$(scan_marker)
  if [ -n "$leftover" ]; then
    note "✗ $1：发现带标记的残留进程："
    printf '%s\n' "$leftover"
    fail=1
  fi
}

# 会话 pid 与会话里那个后台任务的 pid 都要核对：只杀 shell、不杀进程组是过不了的
pids_of() {
  awk '{ for (i = 1; i <= NF; i++) if ($i ~ /^(pid|background|escape)=[0-9][0-9]*$/) { split($i, pair, "="); print pair[2] } }'
}

note "== 构建探针 =="
cargo build -q -p seanbot-core --example bash_session_probe || exit 1

for mode in drop exit closeall; do
  note ""
  note "== 场景 ${mode}：开 2 个会话（各带 sleep 300 后台任务）后以该方式退出 =="
  out=$(cargo run -q -p seanbot-core --example bash_session_probe -- "$mode" 2 2>/dev/null)
  printf '%s\n' "$out"
  pids=$(printf '%s\n' "$out" | pids_of | tr '\n' ' ')
  note "   会话 pid：$pids"
  # shellcheck disable=SC2086
  check_pids_gone "$mode" $pids
  check_no_leftover "$mode"
  note "   ✓ 无残留"
done

# 主动脱离进程组（setsid）的进程：进程组杀不到它，要靠标记扫描 + shell 的 EXIT 陷阱
if command -v setsid >/dev/null 2>&1; then
  ESCAPE='setsid sleep 300 >/dev/null 2>&1 & echo $!'
elif command -v python3 >/dev/null 2>&1; then
  ESCAPE='python3 -c "import os; os.setsid(); os.execvp(chr(115)+chr(108)+chr(101)+chr(101)+chr(112), [chr(115)+chr(108)+chr(101)+chr(101)+chr(112), chr(51)+chr(48)+chr(48)])" >/dev/null 2>&1 & echo $!'
else
  ESCAPE=''
fi

if [ -n "$ESCAPE" ]; then
  for mode in drop exit; do
    note ""
    note "== 场景 escape+${mode}：会话里再起一个脱离进程组(setsid)的进程，然后 $mode 退出 =="
    out=$(cargo run -q -p seanbot-core --example bash_session_probe -- "$mode" 2 "$ESCAPE" 2>/dev/null)
    printf '%s\n' "$out"
    pids=$(printf '%s\n' "$out" | pids_of | tr '\n' ' ')
    note "   相关 pid：$pids"
    # shellcheck disable=SC2086
    check_pids_gone "escape+$mode" $pids
    check_no_leftover "escape+$mode"
    note "   ✓ 逃逸进程也被收掉"
  done
else
  note ""
  note "!! 环境里既没有 setsid 也没有 python3，跳过逃逸进程场景"
fi

note ""
note "== 场景 timeout：命令超时后会话必须被关掉（cargo test 断言 + 扫描复核）=="
if cargo test -q -p seanbot-core --test bash_sessions 2>&1 | tail -3; then
  note "   ✓ bash_sessions 测试通过"
else
  note "   ✗ bash_sessions 测试失败"
  fail=1
fi
check_no_leftover "timeout"

note ""
note "== 全局扫描 =="
leftover=$(scan_marker)
if [ -n "$leftover" ]; then
  note "✗ 仍有残留："
  printf '%s\n' "$leftover"
  fail=1
else
  note "✓ 没有任何带标记的残留进程（含会话 shell 与它们启动的后台任务）"
fi

if [ "$fail" -ne 0 ]; then
  note ""
  note "清理验证失败：存在孤儿进程，必须修掉"
  exit 1
fi
note ""
note "全部通过：drop / exit / closeall / timeout 四条退出路径都没有留下孤儿进程"
