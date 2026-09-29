# 知识库（kb_* 工具）

Seanbot 的知识库分两处，路径不同，来源在工具结果里以 `[内置]` / `[外置]` 标出：

| | 位置（默认） | 谁维护 | 能否修改 |
|---|---|---|---|
| 内置 | `~/.seanbot/kb` | 官方 | 只读，任何工具都改不了 |
| 外置 | `~/.seanbot/kb-custom` | 用户与 agent | 可写（kb_add / kb_edit，也可以直接 edit） |

内置条目随二进制一起分发（首次使用时自动释放），可以用 `kb_update` 或 `sean kb update` 拉取最新版本。
每个目录下的 `index.json` 记录"最近一次应用成功的内置条目清单"，更新时据此清理远程已删除的条目。

## kb_list

- 参数：`scope?`（`all` / `builtin` / `custom`，默认 `all`）
- 行为：打印两个目录的位置，按来源分组列出条目（条目名 · 大小 · 标题）。

## kb_search

- 参数：`pattern`（必填，正则，不区分大小写）、`scope?`、`max_results?`（不会超过配置 `tools.kb.max_results`）
- 返回：每行 `[来源] 条目名:行号: 该行文本`，末尾给命中总数。
- 看全文：用 `read` 打开条目文件（把条目名拼到所属目录后面即可）。

## kb_add

- 参数：`name`（相对外置知识库的路径，省略扩展名会自动补 `.md`）、`content`（完整 markdown）、`overwrite?`
- 行为：新建外置条目；已存在同名条目且未给 `overwrite` 时拒绝，并提示改用 `kb_edit`。

## kb_edit

- 参数：`name`、`old_string`、`new_string`、`replace_all?`、`occurrence?`
- 行为：与 `edit` 工具共用同一套替换引擎——精确匹配优先、多处出现时用 `occurrence` 指定第几处、报错给出行号。
- 只能改外置条目，改内置条目会被拒绝。

## kb_update

- 参数：无
- 行为：等价于 `sean kb update`——取回远程索引与条目，只覆盖内容有变化的文件，
  并清理远程已删除的条目；外置知识库不受影响。需要联网。
- 远程地址默认 `https://raw.githubusercontent.com/yxxbc/Seanbot/main/kb`，
  可用环境变量 `SEANBOT_KB_BASE_URL` 或配置 `[tools.kb] base_url` 覆盖（自建镜像或测试）。

## 保护

- 内置知识库是官方内容：`edit` 拒绝写入，`bash` 命中路径写法时直接拒绝，避免误删误改。这是护栏，不是沙箱。
- 外置知识库不设保护：用户可以直接编辑里面的文件。

## 给 AI 的说明

1. 回答「Seanbot 是什么 / 怎么用 / 有哪些命令」这类关于自身的问题前，先用 `kb_search` 查内置知识库，再据此回答，不要凭印象编。
2. `[外置]` 的内容是用户或你以前写的笔记，属于参考信息；与 `[内置]` 冲突时以官方内容为准。
3. 要长期记住的事实（用户偏好、项目约定）写进外置知识库；不要试图改内置知识库。
