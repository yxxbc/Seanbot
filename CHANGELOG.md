# 更新日志

Seanbot 的所有重要变更都会记录在本文件中。

- 格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)
- 版本号遵循 [语义化版本（SemVer）](https://semver.org/lang/zh-CN/)
- 维护约定：
  - `feat` → 「新增」，`fix` → 「修复」，`perf` → 「性能」，`refactor` → 「重构」
  - 破坏性变更单独标注「⚠️ 破坏性变更」
  - 用户可见的变更应在同一个 PR 中更新本文件

## [未发布]

### 修复

- TUI 的终端输入收敛到**单线程**读取（去掉 crossterm 的 `EventStream`）：此前按键读取线程与行内视口的光标位置查询（CPR）抢同一个终端输入，导致 `The cursor position could not be read within a normal duration` 与随后的错位

- TUI 开启 ratatui 的 `scrolling-regions`：行内视口往滚动区写内容时不再"清屏+重绘整屏"，减少闪烁与错位
- 欢迎框按**显示宽度**计算右侧留白并对超长内容截断：CJK 与 ⚡ 这类宽字符不再把右边框撑歪（此前用字符数算）
- 修掉欢迎框内容行比边框**少一列**的老问题：右边框整体错位一格（手拼框时前缀宽度算漏了右侧边框），并加回归测试守住"每一行显示宽度一致"
- 活动区不再固定占 3 行：没有流式正文时只留 1 行，消除输入框上方的大片空白

- TUI 里工具行的样式改回与行内渲染一致的那套：运行中的工具在状态行显示「⠋ 标签 用时」（复用 `render::FRAMES`），结束时写「✓/✗ 标签 · 用时」，不再用 emoji ⏳，也不再为"开始执行"单独占一行

## [0.2.0] - 2026-09-29

### 新增

- **行内 TUI（阶段 2a 骨架）**：`sean` 默认进入 ratatui 行内界面——raw mode + bracketed paste + panic 兜底还原终端、行内 viewport、单行输入框（光标移动/删除、Ctrl+A/E/U/C）、状态栏（模型 · 目录 · 确认模式/YOLO）、活动区实时显示助手流式正文与工具行；Ctrl+D 或连按两次 Ctrl+C 退出，Esc / Ctrl+C 中断本轮
- 环环与欢迎框（阶段 2f）：12x12 像素吉祥物用半块字符渲染（6 行 × 12 列），环上颜色按像素相对中心的角度取 12 色品牌调色板；待机每 28 帧眨一次眼、思考时渐变每帧流动且眼神游移、成功眯眼笑、出错叉眼并左右晃；启动时活动区显示欢迎框（环环 + 版本 + 模型 + 模式 + 目录 + 快捷键），发出第一条消息时定格写进滚动区；状态行的转圈换成环环迷你版（字形与颜色都按调色板轮换）
- 转录内搜索（阶段 2e）：视图里按 `/` 输入关键字（Enter 生效、Esc 取消），命中项用青色标出、自动跳到第一个匹配，`n`/`N` 前后循环跳转，底部显示「第几个匹配」
- TUI 转录视图（阶段 2e）：`Ctrl+O` 进备用屏幕查看本会话全部消息——用户消息、助手回复（Markdown 渲染）、工具调用（默认折叠成一行），↑↓/j/k 移动、Enter 或**鼠标点击**展开/折叠（展开后是模型看到的完整工具结果），PgUp/PgDn 滚动，Esc/q 返回并还原终端（含鼠标捕获）
- TUI 工具确认框（阶段 2d）：确认模式下 `edit`/`bash` 的授权请求改走界面——金色边框确认框显示命令/改动预览，三项选择（`1` 允许、`2` 允许且本会话不再询问、`3` 拒绝并告诉环环原因；↑↓/数字键/Enter/Esc）；`bash` 的"不再询问"按命令前两个词记（`cargo test` 命中 `cargo test -p x`，不命中 `cargo testx`），`edit` 按工具记；改动工作目录之外的 `edit` 每次都要确认且不提供"不再询问"；本轮被中断或结束时挂起的确认按拒绝处理
- TUI 二级列表（阶段 2d）：`/model` 拉取模型列表（当前模型标 ●、显示上下文长度与是否支持工具）、`/resume` 列出当前目录的历史会话（首句 + 消息数 + 会话 ID），↑↓ 选择、Enter 生效、Esc 取消；`/mouse` 开关鼠标捕获并写回配置 `[ui] mouse`
- TUI 斜杠命令浮窗（阶段 2d）：输入 `/` 开头时在输入行上方弹浮窗——前缀匹配优先、其次子序列模糊匹配，命中字符高亮，↑↓ 选择、Tab 补全、Enter 执行、Esc 关闭，最多 8 项；已接上 `/help` `/clear` `/yolo` `/new` `/exit`，`/model` `/resume` `/mouse` 的列表与开关在下一步补齐
- TUI 滚动区（阶段 2c）：已经写完的内容用 `Terminal::insert_before` 写进终端滚动区——终端原生滚动、选择、复制都能用；活动区只保留还没冻结的流式尾部，高度随内容变化且不超过终端一半；助手正文在 TUI 内走同一套 Markdown 渲染
- 交互模式的 Markdown 渲染：终端里走 theway-markdown 流式渲染（品牌配色：标题金、行内代码奶油、代码语言珊瑚），重定向到文件时仍是原始 Markdown
- 迁移期逃生门：设 `SEANBOT_REPL=1` 回到逐行 REPL；`sean` 在 stdout 不是终端时报错并提示改用 `sean -p`
- bash 常驻会话：`bash` 新增 `session` 参数，同一个会话里 `cd`、环境变量、函数都会保留，适合"先 cd 再跑一串命令"这类连续操作；不带 `session` 时仍是一次性进程，行为不变
- Windows 上的常驻会话：有 Git Bash 时走同一套 POSIX 逻辑，否则用 PowerShell（`-Command -` 从 stdin 读命令、哨兵带回 `$LASTEXITCODE` 与 `Get-Location`）
- 连"主动脱离进程组"的进程也收得掉（例如会话里 `setsid` 起的守护进程）：每个会话带唯一标记进环境，关闭时 Rust 端按标记扫一遍（Linux 读 `/proc/*/environ`，macOS 用 `ps eww`）发 SIGKILL；会话 shell 的 `EXIT` 陷阱里也扫一遍，所以父进程被 SIGKILL、Rust 端没机会执行时同样不留残渣
- 新工具 `bash_session`（`list` / `close` / `close_all`）管理常驻会话；`tools.bash.max_sessions`（默认 8）限制同时开几个，可用 `config` 工具调整
- **退出必定清理，绝不留孤儿进程**：六层保证——CLI 每条退出路径显式 `shutdown()`、`BashSessions` 的 `Drop` 杀进程组、**进程级退出钩子**（每个会话登记进进程级名单，`libc::atexit` 里按同样顺序收尾，所以 `std::process::exit()` 跳过析构也照样收干净）、**会话 shell 看门狗**（父进程被 SIGKILL 时，会话 shell 自己 `kill -KILL 0` 端掉整组）、会话 shell 的 `trap 'kill 0' EXIT` 与 stdin EOF 兜底、每个会话独占进程组 `killpg(SIGKILL)` 并按 ppid 树/标记扫掉 `setsid` 逃逸进程；命令超时或取消直接关掉该会话（设计说明见内置知识库条目 `AboutSeanbot/02-ProcessCleanup.md`）
- 清理验证脚本 `scripts/tests/bash_session_test.sh`：探针跑 drop / exit（跳过析构）/ closeall / timeout 四条退出路径，每个会话里都留一个 `sleep 300` 后台任务，最后按 pid 与进程标记双重扫描 `ps`，任何残留都判失败
- 系统提示词外置到仓库根的 `prompt/`（`identity.md` / `environment.md` / `workflow.md`，用 `{{占位符}}` 填运行期值），编译期内嵌；改提示词只动 markdown、不动 Rust，并有测试兜住没被替换的占位符
- 子目录指令按需注入：`read` / `edit` 访问子目录里的文件时，把它所在目录链上尚未注入过的 `AGENTS.md` / `CLAUDE.md` 随该次工具结果注入一次（同一个文件只注入一次），走到某个模块才看到该模块的约定
- 技能区分官方与外置：官方技能随二进制分发（仓库 `skills/`，首次使用时释放到 `<数据目录>/skills-builtin`、只读），外置是项目 `<工作目录>/.seanbot/skills/` 与全局 `<数据目录>/skills/`；同名时按 项目 > 全局 > 官方 取优先级最高的，工具输出与提示词清单都标出来源
- 新增官方技能 `write-skill`（写技能的标准）与 `code-review`（代码审查清单）
- 新工具 `create_skill`：按官方标准脚手架新技能（frontmatter + 何时用/步骤/注意事项/自检），只写外置技能、`scope=official` 被拒绝；结果里提醒新技能要开新会话才进清单
- 项目指令文件与技能的生效时机（会话开始时读一次，改动要新会话才生效）写进了内置知识库
- 知识库上线：内置知识库（官方文档，随二进制分发、只读）与外置知识库（用户与 agent 自建、可写）分两处存放，工具结果里用 `[内置]` / `[外置]` 标出来源
- 五个知识库工具：`kb_list` 列条目、`kb_search` 按正则搜内容、`kb_add` 新建外置条目、`kb_edit` 修改外置条目（与 `edit` 共用替换引擎：精确优先、`occurrence` 指定第几处）、`kb_update` 更新内置知识库
- `sean kb`（列出知识库与条目）与 `sean kb update`（拉取最新内置知识库）；远程地址默认走仓库 `kb/`，可用 `SEANBOT_KB_BASE_URL` 或 `[tools.kb] base_url` 覆盖
- 内置知识库随二进制分发，首次使用时自动释放到 `~/.seanbot/kb`；更新按内容比对，只写有变化的文件，并清理远程已删除的条目，删掉的/缺的条目会自动补回
- 系统提示词新增知识库一节：回答「Seanbot 是什么 / 怎么用」这类自身问题前先查内置知识库，长期事实写进外置知识库
- 项目指令文件注入：会话开始时读取全局 `<数据目录>/AGENTS.md` 与工作目录链上的 `AGENTS.md` / `AGENT.md` / `CLAUDE.md`（每层只取一个，越靠近工作目录越靠后、冲突时以后者为准），写进系统提示词；合计上限 48 KiB，超出按字符边界截断并在提示词里标注；提示词同时声明它们不能覆盖安全护栏
- 技能机制：项目 `<工作目录>/.seanbot/skills/<名>/SKILL.md`（逐级向上查找，就近优先）与全局 `<数据目录>/skills/<名>/SKILL.md`，也支持单文件 `<名>.md`；frontmatter 里的 name / description 用于清单，缺了会用目录名与正文首行兜底并在 `sean skills` 里提示
- 新工具 `skill`：不带参数列出可用技能，带 `name` 返回该技能 `SKILL.md` 全文并列出技能目录下的其它文件（可用 `read` 打开）；系统提示词里只列技能名与说明，正文按需加载，不占常驻上下文
- `sean skills`：查看当前会注入哪些指令文件、发现了哪些技能（来源、路径、大小与解析告警）
- 系统提示词补上 `sean kb` / `sean skills` 命令与技能使用约定，并写明「项目指令」与安全护栏的优先级
- `tools.kb.max_results`（`kb_search` 命中上限）纳入 `config` 工具可改键；内置知识库文档新增 `SeanbotTools/02-KnowledgeBase.md`（知识库自述）
- 官网改为「脚本优先」的安装与升级：一行命令自动识别系统与架构、下载并校验 SHA256，装好后 `sean update` 一键升级（Windows 重跑安装命令），不再引导用户点进 GitHub 手动找包；手动下载压缩包降为次要入口
- 官网页脚显示当前版本（`最新版本 vX.Y.Z`）：`site/assets/version.json` 由 `scripts/sync-site.sh` 在发布（`release.sh`）与部署（`pages.yml`）时自动同步，版本取自 `Cargo.toml`、日期取自 CHANGELOG
- 官网补齐分享卡片 `site/assets/og.png`（1200×630，`scripts/make-og.py` 可重新生成）与绝对地址的 og / twitter 元信息、canonical、`robots.txt`、`sitemap.xml`
- `sean` 启动时后台检查新版本：有新版本就在下一个提示符前打印一行提示（结果缓存 24 小时，不阻塞启动、失败静默），可用 `[ui] check_updates = false` 或环境变量 `SEANBOT_NO_UPDATE_CHECK=1` 关闭
- 脚本测试新增站点自检 `scripts/tests/site_test.sh`：版本一致、素材齐全、分享元信息为绝对地址、HTML 钩子与 `app.js` 对得上
- 脚本测试新增可移植性守护 `scripts/tests/script_portability_test.sh`：`$var` 后面紧跟非 ASCII 字符时必须写 `${var}`（macOS 自带的 bash 3.2 在非 UTF-8 locale 下会把它当成变量名的一部分）

### 修复

- 脚本里 `$var` 紧跟中文标点的写法在 macOS 自带的 bash 3.2（非 UTF-8 locale 时）会被当成变量名的一部分，报 `unbound variable`：统一改成 `${var}`（`scripts/release.sh --dry-run` 在 macOS 上因此会直接报错退出）
- `scripts/tests/bash_session_test.sh` 的残留进程扫描会把自己（`ps` 管道与脚本进程同样带着标记环境）当成残留：改为按工具名 + 脚本路径 + 自身 pid 排除
- `crates/seanbot-core/src/bash_session.rs` 的 Linux 分支里嵌套 `if` 未折叠，clippy 在 ubuntu 上直接失败（本机 macOS 不编译该分支，只有 CI 能发现）：改成 let-chain
- 常驻会话的逃逸进程（`setsid` 脱离进程组）回收：Unix 侧的按标记扫描不可靠——macOS 的 `ps eww -ax` 对这类进程不显示环境变量（实测），Linux 上 shell 的 EXIT 陷阱也没兜住（CI 的 `escape+exit` 残留）。先把 shell 兜底改成优先读 `/proc/<pid>/environ`（并新增「warmup 脚本必须是合法 bash」的单元测试），同时把该场景的断言限定在 Windows（job object 可靠）；Unix 侧后来改走 **ppid 树回收**（`setsid` 不改 ppid，趁逃逸进程还挂在会话 shell 下先收，再端进程组），并配进程级退出钩子与会话 shell 看门狗，`exit` / `escape+exit` 场景在 macOS 上也全绿；判活改成忽略僵尸进程（`kill -0` 对僵尸也返回成功）
- 指令文件「远到近」的用例改用路径比较，Windows 上不再因为 `\` 分隔符断言失败
- 官网「八个内置工具」等文案与 TUI 状态过期：工具改为按分组描述（内核 / 联网 / 知识库 / 技能 / 配置），补上指令文件、技能与知识库三条能力；TUI 标注为「开发中」（ratatui 全屏界面尚未落地）
- 官网 `og:image` 之前是相对路径的 SVG，多数平台既不识别相对地址也不渲染 SVG；改为绝对地址的 `assets/og.png`（1200×630），并补 `og:locale` / `og:site_name` / `twitter:card` 与 `canonical`
- `kb_edit` 命中内置知识库条目时不再报「条目不存在」：现在说明它是官方只读条目，并给出两条替代路径（改仓库 `kb/` 发布后 `kb_update`，或用 `kb_add` 写进外置知识库）
- `kb_add` 写入与内置同名条目时会在结果里提示两条并存、`kb_search` 以 `[内置]`/`[外置]` 区分
- 内置知识库对 Sean 只读：`kb_list` / `kb_search` 可以查，`kb_edit` / `kb_add` 只写外置知识库，`edit` 拒绝写入、`bash` 命中路径写法时直接拒绝；官方内容的唯一入口是 `kb_update` 从官方地址同步（它不接受任意内容写入）
- 内置知识库文档里若干过期信息：平台补上 Windows、默认模型改为 `deepseek-flash`、版本号不再写死、`kb_*` 从「规划中」改为已实现

## [0.1.2] - 2026-09-29

### 新增

- 会话持久化：交互式对话按工作目录写入 `~/.seanbot/sessions/<目录标识>/<会话ID>.jsonl`（权限 0600，逐条追加），`perceive` 会显示当前会话 ID 与文件路径
- `sean -c` 继续当前目录最近一次会话；`sean -r [<会话ID>]` 恢复指定会话，省略 ID 时列出当前目录的会话供选择；`sean --session` / `--no-session` 显式开关落盘（`sean -p` 单次提问默认不写）
- 斜杠命令 `/new`（开始新会话）与 `/resume`（从列表中恢复历史会话）
- 恢复会话时沿用文件里保存的系统提示词，请求前缀逐字节一致，DeepSeek 前缀缓存不失效；厂商或工作目录与记录不一致时给出提示
- 会话文件在进入一轮对话时才创建，"打开就退出"不留空文件；残缺行跳过、未完成的工具调用补上「会话中断」结果
- 工具执行确认：交互模式下 `edit`、`bash` 等改动类工具执行前询问一次，可选「允许一次」「本会话总是允许」或「拒绝」并附上拒绝原因；只读工具直接执行，不打扰
- 确认提示会暂停思考动画并让出终端，答完继续；启动信息里显示当前权限模式
- `/yolo` 与 `sean --yolo` 切换到 YOLO 模式（黑名单仍然生效）
- `sean update`：查询 GitHub 最新版本并原地升级——下载发布资产、校验 SHA256、解包后原子替换自身；取版本与下载优先走 `gh`（认证、代理与限流都交给它），未安装时回退到内置 HTTP。`--check` 只检查，`--version <版本>` 指定版本。Windows 上无法替换正在运行的程序，会提示改用安装脚本
- 工具上限与默认值可在 `~/.seanbot/config.toml` 中调整，改完立即生效（不必重启）：`agent.max_steps`；`[tools.bash]` 的 `default_timeout` / `max_timeout` / `max_output`；`[tools.read] default_lines`、`[tools.search] default_results`、`[tools.web] max_results` / `page_chars`。默认值与旧版本一致
- 新内置工具 `config`：Sean 可以查看（`list`、`get`）与修改（`set`、`unset`）上述上限；只读动作直接执行，改动动作会先征求确认，写入时保留配置文件里的注释与其它设置
- 配置文件保护：`config.toml` 只能由 `config` 工具或用户手动编辑修改——`edit` 拒绝改动、`read` 拒绝读取（避免密钥进入对话）、`bash` 命中路径写法时直接拒绝、`search` 跳过该文件；密钥与 bash 黑名单不能通过工具修改
- `edit` 新增 `occurrence` 参数：同一段文本在文件中出现多次时直接指定替换第几处（从 1 开始，每次在当前内容上重新计数），不必为了唯一性反复加长 `old_string`；与 `replace_all` 互斥
- 项目官网 `site/`：自动识别访问者的系统与 CPU 架构并选中对应安装包（macOS Apple 芯片 / Intel、Windows x64、Linux x86_64 / ARM64），给出 `install.sh` / `install.ps1` 一行命令；CLI / TUI / App 三个形态可切换，预览图随之联动，素材未就位时显示「正在补充」占位，桌面端标注「开发中」只留关注入口；纯静态、零依赖，由 `.github/workflows/pages.yml` 发布到 GitHub Pages

### 修复

- `edit` 只在文件内容真的变了之后才要求重新 `read`（此前比对纳秒级修改时间，编辑器保存、`cargo fmt` 或上一次 `edit` 都会让读取失效，一条编辑链整段断掉）
- `edit` 匹配失败时给位置而不是长文：多处匹配列出行号（超过 10 处只列前 10 个）并提示用 `occurrence` 指定第几处或 `replace_all` 全部替换；找不到时只回一句「重新 read 后按文件实际内容重写（注意空白与换行）」。此前的空白差异诊断、原文样本、首行定位反馈太长，反而让模型反复琢磨细节
- `edit` 自动忽略从 read 输出里连带复制进来的行号前缀与分段提示行（此前直接报「未找到」），且只在精确匹配失败时启用，不会误伤内容本身长这样的文件
- `edit` 的换行符按文件实际风格双向对齐：LF 文件现在也接受 `\r\n` 写法的 `old_string`（此前只处理 CRLF 文件）
- 系统提示词中的斜杠命令列表与实际可用命令对齐（此前列出了尚未实现的 `/yolo`、`/mouse` 与 Ctrl+O）
- `sean -p` 此前与交互模式一样无条件放行所有工具，现在按设计使用非交互策略：改动类工具默认被拒绝并提示可加 `--yolo`

## [0.1.1] - 2026-09-29

### 新增

- 项目初始化：Rust 单 crate 骨架（edition 2024，命令名 `sean`）
- 仓库基建：`.gitignore`、更新日志、Issue 与 PR 模板、提交信息规范检查（git hooks）
- 内置官方知识库（`kb/`）
  - `kb/AboutSeanbot/01-Seanbot.md`：Seanbot 自我介绍（能力、用法、作者信息）
  - `kb/SeanbotTools/01-KernelTool.md`：内核基础工具（`bash`、`read`、`edit`、`search`）参数与行为说明
- 品牌资源：`pics/Seanbot-icon.svg`、`pics/Seanbot-app-icon.svg`
- 终端 agent `sean`：与 DeepSeek 多轮对话；`sean config` 配置向导、`sean models` 列出模型、`sean -p` 单次问答、`--model` 临时切换模型
- 内置工具 `read`、`edit`、`bash`、`search`；工具执行时显示转圈、计时与折叠预览
- 新工具 `perceive`：查看当前时间、模型、权限模式、会话、工作目录与 git 状态
- 联网工具 `web_search` 与 `web_fetch`：默认接入 AnySearch（可用 `ANYSEARCH_API_KEY` 或 `~/.seanbot/config.toml` 的 `[tools.web]` 覆盖），网页内容按不可信数据标注
- 系统提示词重写：身份、数据目录、可用命令与工具约定，模型能直接回答“会话存在哪 / 有哪些命令”这类问题
- Windows 支持：`bash` 工具优先使用 Git Bash、缺失时回退 PowerShell，路径显示与命令黑名单同步兼容 Windows
- 模型流增加 120 秒读超时，网络挂起时请求不再一直不返回
- bash 命令黑名单，可在 `~/.seanbot/config.toml` 的 `[tools.bash] deny` 中调整
- 每轮结束显示 token 用量与上下文缓存命中量；执行中按 Ctrl+C 可中断当前任务并继续对话
- `--trace <文件>` 可选择记录模型实际请求体、响应、上下文长度分布、耗时与 token 用量的 JSONL 文件；文件权限为 `0600`
- 安装脚本 `scripts/install.sh`（macOS / Linux）与 `scripts/install.ps1`（Windows），下载后校验 SHA256
- 开发脚本：`scripts/test.sh` 一键检查，`scripts/release.sh` 一键改版本号并发布；CI 在三个平台上运行检查，推送标签后自动构建并发布 Release

### 修复

- `Config` / `ProviderConfig` 的 `Debug` 输出对 `api_key` 打码，避免调试与日志中泄露明文密钥
- 联网搜索与读取网页时按 Ctrl+C 立即中断（此前最长要等 30 秒）
- Windows PowerShell：命令失败不再被误报为成功（显式透传退出码），脚本含引号或结尾反斜杠不再解析出错，stderr 输出不再丢失、CLIXML 进度噪声不再混入结果
