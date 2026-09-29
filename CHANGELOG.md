# 更新日志

Seanbot 的所有重要变更都会记录在本文件中。

- 格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)
- 版本号遵循 [语义化版本（SemVer）](https://semver.org/lang/zh-CN/)
- 维护约定：
  - `feat` → 「新增」，`fix` → 「修复」，`perf` → 「性能」，`refactor` → 「重构」
  - 破坏性变更单独标注「⚠️ 破坏性变更」
  - 用户可见的变更应在同一个 PR 中更新本文件

## [未发布]

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

### 修复

- `edit` 只在文件内容真的变了之后才要求重新 `read`（此前比对纳秒级修改时间，编辑器保存、`cargo fmt` 或上一次 `edit` 都会让读取失效，一条编辑链整段断掉）
- `edit` 匹配失败时不再只回一句「未找到 / 出现 N 次」：多处匹配会列出每一处出现的行号与所在行，内容对不上会指出差在行尾空白、缩进宽度、全角空格还是换行符，并给出第一处不一致的原文（空白以 `·`/`→` 显形），只匹配到首行时报告首行位置
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
