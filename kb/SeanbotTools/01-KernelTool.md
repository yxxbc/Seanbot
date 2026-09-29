# Kernel tool（内核基础工具）

Seanbot 内核（seanbot-core）内置的四个基础工具，agent 通过它们操作本地文件与命令：

- `bash` —— 执行命令
- `read` —— 读取文件
- `edit` —— 修改文件
- `search` —— 搜索代码和文件

## 说明

### bash

执行本地命令。

- 参数：`command`（必填）、`timeout?`、`session?`
- 行为：以 `bash -c` 执行（无 bash 时改用 `sh -c`）；工作目录为会话工作目录；默认超时 120 秒，上限 600 秒；超时或取消时杀掉整个进程组；stdout 与 stderr 合并；输出超过 30000 字符时保留首尾各 15000 并标注省略量；返回退出码。每次调用相互独立，不保留 shell 状态。
- 安全：受 bash 黑名单约束，命中时不执行并返回"命令被黑名单拒绝"。默认拒绝 `rm`、`sudo`、`mkfs`（含 `mkfs.*`）、`dd`、`shutdown`、`reboot`、`chmod -R`、`git push --force`、`git push -f`，可在配置中增删。黑名单是护栏而非沙箱，不能拦截一切绕行写法。
- 常驻会话（`session` 参数）：同一个会话里保留 `cd`、环境变量与函数，适合"先 cd 再跑一串命令"；用 `bash_session` 工具 `list` / `close` / `close_all` 管理。程序退出时（含异常退出）会把会话 shell、它起的后台任务与 `setsid` 逃逸进程一起收掉，机制见内置条目 `AboutSeanbot/02-ProcessCleanup.md`。

### read

读取文件内容。

- 参数：`path`（必填）、`offset?`、`limit?`
- 行为：返回带行号的内容；默认最多 2000 行；单行超过 2000 字符截断；二进制文件报错。读取成功后把文件的内容指纹（字节数 + 哈希）记入 ReadTracker，供 `edit` 判断内容有没有变。

### edit

修改或新建文件。

- 参数：`path`、`old_string`、`new_string`（均必填）、`replace_all?`、`occurrence?`
- 行为：`old_string` 必须在文件中唯一出现；同一段文本出现多次时用 `occurrence` 指定第几处（从 1 开始，每次在当前内容上重新计数），或用 `replace_all` 全部替换（两者互斥）；`old_string` 为空且目标文件不存在 → 新建文件并自动创建父目录；修改已存在的文件前，必须在本会话中 `read` 过该文件且内容未变（只比对内容，不看修改时间：编辑器保存、`cargo fmt`、上一次 edit 都不会让读取失效，内容没变时读一次可连续编辑），否则拒绝并提示重新 `read`；写入后更新 ReadTracker。
- 容错：报错刻意保持一行，避免模型反复琢磨细节。`old_string` 找不到时只提示重新 `read` 后按文件实际内容重写（注意空白与换行）；多处匹配时列出行号并提示用 `occurrence` 指定第几处或 `replace_all` 全部替换；`occurrence` 越界会报出实际出现次数与行号。
- 行号前缀：从 read 输出里连带复制进来的「行号 + TAB」前缀与分段提示行 `(文件共 N 行，本次显示第 X-Y 行)` 会在精确匹配失败时被自动忽略，并在结果里说明（精确匹配优先，不会误伤内容本身长这样的文件）。
- 换行符：跟随文件的实际换行风格双向对齐——CRLF 文件接受 `\n` 写法的 `old_string`，LF 文件也接受 `\r\n` 写法。

### search

搜索代码和文件。

- 参数：`pattern?`、`glob?`、`path?`、`max_results?`，至少提供一个
- 行为：仅提供 `glob` 时按文件名列出路径；提供 `pattern` 时用正则搜索内容，输出 `path:line: text`；自动遵守 `.gitignore`；默认最多返回 200 条，超出时注明。

## 给 AI 的说明

1. 工具名与参数名保持英文，不要翻译；向用户解释时用中文。
2. 四个基础工具是内核内置工具，不可更改，也不允许注册同名工具覆盖它们。
3. 相对路径一律以会话工作目录为基准解析。
4. 工具失败、被拒或超时不会中断任务：错误会作为工具结果返回，应据此修正后重试。
5. 知识库工具（`kb_list`/`kb_search`/`kb_add`/`kb_edit`/`kb_update`）已经可用，见内置条目 `SeanbotTools/02-KnowledgeBase.md`；人格工具（`per_*`）属于规划功能，当前版本尚未提供。
