# 常驻 bash 会话的进程清理

`bash` 工具带 `session` 参数时会留一个常驻 shell（`bash -s`）：同一个会话里 `cd`、环境变量、函数都保留。
它比一次性的 `bash -c` 活得久，因此必须保证——**只要 Seanbot 退出，会话里的进程一个不剩**：会话 shell 本身、它起的后台任务（`cmd &`）、甚至主动 `setsid` 脱离进程组的守护进程。
验收脚本按 pid 与进程标记双重扫描 `ps`，任何残留都判失败。

## 六层保证

| 层 | 覆盖的退出方式 | 机制 |
| --- | --- | --- |
| 1 显式收尾 | 程序里每条退出路径、`bash_session close` / `close_all` | 逐个 `Session::shutdown()` |
| 2 `Drop` | 正常返回、报错返回、panic 展开 | 会话集合的最后一个持有者收尾 |
| 3 进程级退出钩子 | `std::process::exit()`（跳过所有析构） | 会话登记进进程级名单，`libc::atexit` 钩子里按第 1 层同样的顺序收 |
| 4 会话 shell 看门狗 | 父进程被 SIGKILL（连钩子都跑不到） | 会话 shell 后台起一个子 shell 盯着父进程与自己，谁没了就 `kill -KILL 0` 端掉整组 |
| 5 `EXIT` 陷阱 + EOF 兜底 | 会话 shell 自己退出时顺手清一遍 | shell 退出前按 ppid 树 + 标记扫一遍，再 `kill 0` |
| 6 进程组 + 逃逸进程回收 | 关闭、超时、取消 | 每个会话独占进程组 → `killpg(SIGKILL)`；`setsid` 逃逸的按 ppid 树收，Linux 再按标记扫 `/proc/<pid>/environ` 补 |

收尾顺序固定为 **ppid 树 → 进程组 → 标记扫描**：反过来逃逸进程会被 reparent 到 init（pid 1），ppid 树就断链了。

## 为什么需要这么多层

- `std::process::exit()` 跳过所有析构（panic=abort、被 SIGKILL 同理），只靠 `Drop` 收不干净。
- 指望子 shell「读到 stdin EOF 就自杀」不可靠：非交互 shell 里的异步列表（`cmd &`）按 POSIX 规定**先把 stdin 接到 `/dev/null`**，后台作业根本不吃 EOF——会话 shell 自己是退出了，它 fork 出来的作业却成了孤儿。
- macOS 内核不允许读别的进程的环境变量（`ps eww` 与第三方进程工具走的同一套系统调用，都读不到），所以「按环境变量标记找进程」只有 Linux（读 `/proc/<pid>/environ`）能用；跨平台可靠的是按 ppid 找——`setsid` 不会改 ppid。
- 顺序很关键：必须先按 ppid 树收，再端进程组；反过来的话逃逸进程已经被 reparent，再也找不回来。

## 用户可以观察到的

- `bash_session` 工具：`list` 看当前会话、`close <名字>` 关一个、`close_all` 全关；同时开几个由 `tools.bash.max_sessions`（默认 8）限制，可用 `config` 工具调整。
- 命令超时（默认 120 秒）或按 Ctrl+C 取消时，该会话直接关闭，不留后台残留。
- 会话是「尽力而为的安全清理」而不是沙箱：既主动 `setsid` 又清空环境变量、还把自己 reparent 出去的教科书式守护进程化，仍可能漏网。

## 怎么自查

```bash
bash scripts/tests/bash_session_test.sh   # 六个场景：drop / exit / closeall / escape+drop / escape+exit / timeout
```

脚本会构建探针（`crates/seanbot-core/examples/bash_session_probe.rs`），让每个会话里各留一个 `sleep 300` 后台任务与一个 `setsid` 逃逸进程，然后分别用不同方式退出，最后按 pid 与进程标记扫描 `ps` 复核。

## 给 AI 的说明

1. 用户问「会不会留下孤儿进程 / 后台任务会不会被清掉 / 常驻会话怎么关」时，依据本文件回答：程序退出时会清理，而且不只杀会话 shell，连同后台任务与 `setsid` 逃逸的进程一起收；具体机制见上面六层。
2. 需要主动关会话时用 `bash_session` 工具，不要用 `kill` / `pkill` 直接动进程。
3. 不要把清理能力说成沙箱：黑名单是护栏，清理是尽力而为。
4. 本文件属于**内置官方知识库**：不要改写或删除，更新通过 `sean kb update` 从远程获取。
