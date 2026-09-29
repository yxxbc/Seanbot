//! 知识库工具：kb_list / kb_search / kb_add / kb_edit / kb_update。
//!
//! - 内置知识库（官方文档、只读）：首次使用时从二进制释放，`kb_update` 拉取最新
//! - 外置知识库（用户与 agent 自建、可写）：`kb_add` 新建、`kb_edit` 修改
//!
//! `kb_edit` 复用 edit 工具的替换引擎（精确优先、occurrence、诊断），两处语义一致。

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use seanbot_provider::ToolSpec;
use serde_json::{Value, json};

use crate::{
    kb::{self, Scope},
    tool::{Risk, Tool, ToolContext, ToolError, ToolOutput, opt_bool, opt_str, opt_u64, str_arg},
    tools::edit::{diff_stats, plan_edit},
};

pub struct KbListTool;

#[async_trait]
impl Tool for KbListTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "kb_list".into(),
            description: "列出知识库条目。内置知识库是官方文档（只读，介绍 Seanbot 自身与内置工具）；外置知识库是用户或你自建的笔记（可写）。回答关于 Seanbot 自身的问题前，先用本工具看有哪些条目。".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "scope": {"type": "string", "enum": ["all", "builtin", "custom"], "description": "只看内置、只看外置，或两者都看；默认 all"}
                }
            }),
        }
    }

    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }

    fn title(&self, args: &Value) -> String {
        format!("kb_list {}", scope_label(args))
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let scope = scope_of(&args)?;
        let (builtin, custom) = dirs(ctx)?;
        let entries = kb::list(&builtin, &custom, scope).map_err(failed)?;

        let mut lines = vec![
            format!("内置：{}（官方维护，只读）", builtin.display()),
            format!("外置：{}（你或用户自建，可写）", custom.display()),
        ];
        for group in [Scope::Builtin, Scope::Custom] {
            if scope != Scope::All && scope != group {
                continue;
            }
            let items: Vec<_> = entries.iter().filter(|e| e.scope == group).collect();
            lines.push(String::new());
            lines.push(format!("{}（{} 条）", group.label(), items.len()));
            if items.is_empty() {
                lines.push(match group {
                    Scope::Custom => "- （空）新建条目用 kb_add".to_string(),
                    _ => "- （空）内置条目还没释放，试试 kb_update".to_string(),
                });
                continue;
            }
            for entry in items {
                lines.push(format!(
                    "- {} · {} · {}",
                    entry.name,
                    human_bytes(entry.bytes),
                    entry.title
                ));
            }
        }
        Ok(out(lines.join("\n"), &format!("列出 {} 条", entries.len())))
    }
}

pub struct KbSearchTool;

#[async_trait]
impl Tool for KbSearchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "kb_search".into(),
            description: "在知识库里按正则搜索内容（不区分大小写），返回 来源、条目名、行号与该行文本。内置知识库有官方对 Seanbot 自身与内置工具的说明，回答这类问题前先搜一遍；外置知识库是用户与你自建的笔记。看全文用 read 打开条目文件。".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "搜索式（正则），例如「会话」或 session|cache"},
                    "scope": {"type": "string", "enum": ["all", "builtin", "custom"], "description": "搜索范围，默认 all"},
                    "max_results": {"type": "integer", "description": "最多返回多少条命中"}
                },
                "required": ["pattern"]
            }),
        }
    }

    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }

    fn title(&self, args: &Value) -> String {
        args.get("pattern")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let scope = scope_of(&args)?;
        let pattern = str_arg(&args, "pattern")?;
        if pattern.trim().is_empty() {
            return Err(ToolError::InvalidArgs("pattern 不能为空".into()));
        }
        let max = max_results(&args, ctx)?;
        let (builtin, custom) = dirs(ctx)?;
        let hits = kb::search(&builtin, &custom, scope, pattern, max).map_err(kb_err)?;
        if hits.is_empty() {
            return Ok(out(
                format!(
                    "知识库里没有匹配「{pattern}」的内容。换个关键词，或用 kb_list 看看有哪些条目。"
                ),
                "0 条命中",
            ));
        }
        let mut lines: Vec<String> = hits
            .iter()
            .map(|hit| {
                format!(
                    "[{}] {}:{}: {}",
                    hit.scope.label(),
                    hit.name,
                    hit.line,
                    hit.text
                )
            })
            .collect();
        lines.push(String::new());
        lines.push(format!(
            "共 {} 条（上限 {max}）。看全文：read 打开条目文件；改外置条目：kb_edit",
            hits.len()
        ));
        Ok(out(lines.join("\n"), &format!("命中 {} 条", hits.len())))
    }
}

pub struct KbAddTool;

#[async_trait]
impl Tool for KbAddTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "kb_add".into(),
            description: "往外置知识库新增一个 markdown 条目。条目名是相对路径（例：notes/rust-errors.md，省略扩展名会自动补 .md），内容用 markdown 写，首行建议是一级标题。已存在同名条目时会被拒绝——改内容用 kb_edit。内置知识库只读，本工具不会碰它。".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "条目名（相对外置知识库的路径），如 notes/rust-errors.md"},
                    "content": {"type": "string", "description": "条目的完整 markdown 内容"},
                    "overwrite": {"type": "boolean", "description": "已存在同名条目时是否整体覆盖，默认 false"}
                },
                "required": ["name", "content"]
            }),
        }
    }

    fn risk(&self) -> Risk {
        Risk::Mutating
    }

    fn title(&self, args: &Value) -> String {
        args.get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let (builtin, custom) = dirs(ctx)?;
        let name = str_arg(&args, "name")?;
        let content = str_arg(&args, "content")?;
        if content.trim().is_empty() {
            return Err(ToolError::InvalidArgs("content 不能为空".into()));
        }
        let overwrite = opt_bool(&args, "overwrite")?.unwrap_or(false);
        let clash = kb::has_entry(&builtin, name);
        let path = kb::write_custom(&custom, name, content, overwrite).map_err(kb_err)?;
        let entry = relative_name(&custom, &path);
        let mut message = format!(
            "已写入外置知识库条目 {entry}（{} 行）\n路径：{}",
            content.lines().count(),
            path.display()
        );
        if clash {
            message.push_str(
                "\n注意：内置知识库里已有同名条目，两条会并存（kb_search 结果里用 [内置] / [外置] 区分）",
            );
        }
        Ok(out(message, &format!("kb_add {entry}")))
    }
}

pub struct KbEditTool;

#[async_trait]
impl Tool for KbEditTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "kb_edit".into(),
            description: "修改外置知识库里已有条目的内容：old_string 换成 new_string，必须与文件逐字一致、且唯一出现（多处出现时用 occurrence 指定第几处，或用 replace_all 全部替换）。只写外置知识库；内置知识库由官方维护，任何工具都不能改，要更新它用 kb_update。".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "条目名（相对外置知识库的路径）"},
                    "old_string": {"type": "string", "description": "要被替换的原文，必须与条目内容逐字一致"},
                    "new_string": {"type": "string", "description": "替换后的内容"},
                    "replace_all": {"type": "boolean", "description": "替换所有出现处，默认 false"},
                    "occurrence": {"type": "integer", "description": "替换第几处出现（从 1 开始）；与 replace_all 互斥"}
                },
                "required": ["name", "old_string", "new_string"]
            }),
        }
    }

    fn risk(&self) -> Risk {
        Risk::Mutating
    }

    fn title(&self, args: &Value) -> String {
        args.get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let (builtin, custom) = dirs(ctx)?;
        let name = str_arg(&args, "name")?;
        let old = str_arg(&args, "old_string")?;
        let new = str_arg(&args, "new_string")?;
        let replace_all = opt_bool(&args, "replace_all")?.unwrap_or(false);
        let occurrence = opt_u64(&args, "occurrence")?.map(|n| n as usize);
        if occurrence.is_some() && replace_all {
            return Err(ToolError::InvalidArgs(
                "occurrence 与 replace_all 不能同时使用".into(),
            ));
        }

        let (path, text) = match kb::read_custom(&custom, name) {
            Ok(found) => found,
            // 名字在外置库里没有、但内置库里有：说清为什么不能改，别只报"条目不存在"
            Err(kb::KbError::NotFound(_)) if kb::has_entry(&builtin, name) => {
                return Err(ToolError::Failed(format!(
                    "{name} 是内置知识库的条目（官方维护、只读），kb_edit 只能改外置知识库。要改官方内容：改仓库 kb/ 下的文件，发布后再执行 kb_update；要记自己的内容：用 kb_add 写进外置知识库（两条会并存，kb_search 用 [内置]/[外置] 区分）"
                )));
            }
            Err(other) => return Err(kb_err(other)),
        };
        let label = relative_name(&custom, &path);
        let plan = plan_edit(&label, &text, old, new, replace_all, occurrence)
            .map_err(ToolError::Failed)?;
        kb::write_entry(&custom, name, &plan.updated).map_err(kb_err)?;

        let (added, removed, preview) = diff_stats(&text, &plan.updated);
        let mut message = format!(
            "已修改外置条目 {label}（替换 {} 处，+{added} -{removed} 行）",
            plan.replaced
        );
        for note in &plan.notes {
            message.push_str(&format!("\n提示：{note}"));
        }
        Ok(ToolOutput {
            content: message,
            summary: format!("+{added} -{removed} 行"),
            preview,
            is_error: false,
        })
    }
}

pub struct KbUpdateTool;

#[async_trait]
impl Tool for KbUpdateTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "kb_update".into(),
            description: "把内置知识库更新到最新（等价于用户运行 sean kb update）：从官方地址取回索引与条目，只覆盖内容有变化的文件，外置知识库不受影响。需要联网。它只能从官方地址同步，不能写入任意内容。".into(),
            parameters: json!({
                "type": "object",
                "properties": {}
            }),
        }
    }

    fn risk(&self) -> Risk {
        Risk::Mutating
    }

    fn title(&self, _args: &Value) -> String {
        "更新内置知识库".to_string()
    }

    async fn call(&self, _args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let (builtin, _) = dirs(ctx)?;
        let url = ctx.config.read(kb::base_url);
        let report = kb::update(&builtin, &url).await.map_err(|e| {
            ToolError::Failed(format!(
                "{e}。检查网络后重试，或让用户手动运行 sean kb update（地址可用 [tools.kb] base_url 覆盖）"
            ))
        })?;

        let mut lines = vec![
            format!("内置知识库：{}", builtin.display()),
            report.summary(),
        ];
        for (name, change) in &report.changed {
            lines.push(format!("- {} {name}", change.label()));
        }
        for name in &report.removed {
            lines.push(format!("- 移除 {name}"));
        }
        Ok(out(lines.join("\n"), &report.summary()))
    }
}

#[cfg(test)]
mod protection_tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, ToolContext) {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = ToolContext::new(dir.path().to_path_buf());
        ctx.kb_builtin = Some(dir.path().join("kb"));
        ctx.kb_custom = Some(dir.path().join("kb-custom"));
        (dir, ctx)
    }

    async fn run(tool: &dyn Tool, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        tool.call(args, ctx).await
    }

    /// 命中内置条目时要说明原因，不能只说"条目不存在"。
    #[tokio::test]
    async fn editing_a_builtin_entry_explains_why() {
        let (_dir, ctx) = setup();
        run(&KbListTool, &ctx, json!({})).await.unwrap(); // 触发释放

        let err = run(
            &KbEditTool,
            &ctx,
            json!({
                "name": "AboutSeanbot/01-Seanbot.md",
                "old_string": "# what is Seanbot?",
                "new_string": "# x"
            }),
        )
        .await
        .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("内置知识库"), "{text}");
        assert!(text.contains("kb_update"), "{text}");
        assert!(!text.contains("条目不存在"), "{text}");

        // 往外置库写同名条目是允许的，但要提示两条会并存
        let out = run(
            &KbAddTool,
            &ctx,
            json!({"name": "AboutSeanbot/01-Seanbot.md", "content": "# 我的版本\n"}),
        )
        .await
        .unwrap();
        assert!(out.content.contains("并存"), "{}", out.content);
    }
}

/// 知识库两个目录；顺带把内嵌的官方条目释放出来（已存在的不覆盖）。
fn dirs(ctx: &ToolContext) -> Result<(PathBuf, PathBuf), ToolError> {
    let (builtin, custom) = ctx.kb_dirs()?;
    kb::ensure_builtin(&builtin).map_err(failed)?;
    Ok((builtin, custom))
}

fn scope_of(args: &Value) -> Result<Scope, ToolError> {
    let raw = opt_str(args, "scope")?.unwrap_or("");
    Scope::parse(raw).ok_or_else(|| {
        ToolError::InvalidArgs(format!(
            "scope 只能是 all、builtin 或 custom，收到「{raw}」"
        ))
    })
}

fn scope_label(args: &Value) -> String {
    args.get("scope")
        .and_then(Value::as_str)
        .unwrap_or("all")
        .to_string()
}

/// 命中上限：模型可以调小，但不能超过配置里的 tools.kb.max_results。
fn max_results(args: &Value, ctx: &ToolContext) -> Result<usize, ToolError> {
    let cap = ctx.config.read(|c| c.tools.kb.max_results).max(1) as usize;
    let asked = opt_u64(args, "max_results")?
        .map(|n| n as usize)
        .unwrap_or(cap);
    Ok(asked.clamp(1, cap))
}

fn out(content: String, summary: &str) -> ToolOutput {
    let preview = content.lines().take(3).map(String::from).collect();
    ToolOutput {
        content,
        summary: summary.to_string(),
        preview,
        is_error: false,
    }
}

fn kb_err(e: kb::KbError) -> ToolError {
    match e {
        kb::KbError::BadName(message) => ToolError::InvalidArgs(message),
        other => ToolError::Failed(other.to_string()),
    }
}

fn failed(e: kb::KbError) -> ToolError {
    ToolError::Failed(e.to_string())
}

fn relative_name(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn human_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{EditTool, ReadTool};
    use std::fs;

    fn setup() -> (tempfile::TempDir, ToolContext) {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = ToolContext::new(dir.path().to_path_buf());
        ctx.kb_builtin = Some(dir.path().join("kb"));
        ctx.kb_custom = Some(dir.path().join("kb-custom"));
        (dir, ctx)
    }

    async fn run(tool: &dyn Tool, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        tool.call(args, ctx).await
    }

    #[tokio::test]
    async fn list_releases_builtin_entries_idempotently() {
        let (_dir, ctx) = setup();
        let out = run(&KbListTool, &ctx, json!({})).await.unwrap();
        assert!(
            out.content.contains("AboutSeanbot/01-Seanbot.md"),
            "{}",
            out.content
        );
        assert!(out.content.contains("外置（0 条）"), "{}", out.content);
        let again = run(&KbListTool, &ctx, json!({})).await.unwrap();
        assert_eq!(again.content, out.content, "重复调用不应重复释放");
    }

    #[tokio::test]
    async fn search_covers_builtin_and_custom() {
        let (_dir, ctx) = setup();
        run(
            &KbAddTool,
            &ctx,
            json!({"name": "notes/a", "content": "# 笔记\n知识库检索目标\n"}),
        )
        .await
        .unwrap();

        let builtin = run(
            &KbSearchTool,
            &ctx,
            json!({"pattern": "Seanbot", "scope": "builtin"}),
        )
        .await
        .unwrap();
        assert!(builtin.content.contains("[内置]"), "{}", builtin.content);
        assert!(!builtin.content.contains("[外置]"), "{}", builtin.content);

        let custom = run(&KbSearchTool, &ctx, json!({"pattern": "知识库检索目标"}))
            .await
            .unwrap();
        assert_eq!(
            custom.content.matches("[外置]").count(),
            1,
            "{}",
            custom.content
        );
        assert!(
            custom.content.contains("notes/a.md:2"),
            "{}",
            custom.content
        );

        let err = run(&KbSearchTool, &ctx, json!({"pattern": "(["}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("正则"), "{err}");
    }

    #[tokio::test]
    async fn add_and_edit_share_the_replace_engine() {
        let (dir, ctx) = setup();
        run(
            &KbAddTool,
            &ctx,
            json!({"name": "notes/b.md", "content": "# B\nfoo\nfoo\n"}),
        )
        .await
        .unwrap();
        let out = run(
            &KbEditTool,
            &ctx,
            json!({"name": "notes/b.md", "old_string": "foo", "new_string": "bar", "occurrence": 2}),
        )
        .await
        .unwrap();
        assert!(out.content.contains("替换 1 处"), "{}", out.content);
        let path = dir.path().join("kb-custom/notes/b.md");
        assert_eq!(fs::read_to_string(&path).unwrap(), "# B\nfoo\nbar\n");

        let err = run(
            &KbEditTool,
            &ctx,
            json!({"name": "notes/b.md", "old_string": "zzz", "new_string": "x"}),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("未找到"), "{err}");

        let err = run(
            &KbEditTool,
            &ctx,
            json!({"name": "notes/b.md", "old_string": "a", "new_string": "b", "occurrence": 1, "replace_all": true}),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("不能同时使用"), "{err}");

        let err = run(
            &KbEditTool,
            &ctx,
            json!({"name": "notes/none.md", "old_string": "a", "new_string": "b"}),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("条目不存在"), "{err}");
    }

    #[tokio::test]
    async fn add_rejects_escapes_and_accidental_overwrite() {
        let (_dir, ctx) = setup();
        let err = run(
            &KbAddTool,
            &ctx,
            json!({"name": "../evil.md", "content": "x"}),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
        let err = run(
            &KbAddTool,
            &ctx,
            json!({"name": "notes/c", "content": "  "}),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("content"), "{err}");

        run(
            &KbAddTool,
            &ctx,
            json!({"name": "notes/c.md", "content": "# C\n"}),
        )
        .await
        .unwrap();
        let err = run(
            &KbAddTool,
            &ctx,
            json!({"name": "notes/c.md", "content": "# C2\n"}),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("kb_edit"), "{err}");
        run(
            &KbAddTool,
            &ctx,
            json!({"name": "notes/c.md", "content": "# C2\n", "overwrite": true}),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn builtin_entries_are_protected_but_custom_ones_are_editable() {
        let (_dir, ctx) = setup();
        run(&KbListTool, &ctx, json!({})).await.unwrap();
        let builtin_file = ctx
            .kb_builtin
            .clone()
            .unwrap()
            .join("AboutSeanbot/01-Seanbot.md")
            .to_string_lossy()
            .into_owned();
        ReadTool
            .call(json!({"path": builtin_file}), &ctx)
            .await
            .unwrap();
        let err = EditTool
            .call(
                json!({"path": builtin_file, "old_string": "# what is Seanbot?", "new_string": "# x"}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("内置知识库"), "{err}");

        run(
            &KbAddTool,
            &ctx,
            json!({"name": "notes/d.md", "content": "# D\nhello\n"}),
        )
        .await
        .unwrap();
        let custom_file = ctx
            .kb_custom
            .clone()
            .unwrap()
            .join("notes/d.md")
            .to_string_lossy()
            .into_owned();
        ReadTool
            .call(json!({"path": custom_file.clone()}), &ctx)
            .await
            .unwrap();
        EditTool
            .call(
                json!({"path": custom_file.clone(), "old_string": "hello", "new_string": "hi"}),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(fs::read_to_string(&custom_file).unwrap(), "# D\nhi\n");
    }

    #[tokio::test]
    async fn read_only_flags_and_update_failure_is_readable() {
        assert!(KbListTool.read_only_call(&json!({})));
        assert!(KbSearchTool.read_only_call(&json!({"pattern": "x"})));
        assert!(!KbAddTool.read_only_call(&json!({"name": "n", "content": "c"})));
        assert!(!KbEditTool.read_only_call(&json!({})));
        assert!(!KbUpdateTool.read_only_call(&json!({})));
        assert_eq!(KbUpdateTool.title(&json!({})), "更新内置知识库");

        let (_dir, ctx) = setup();
        ctx.config
            .update(|c| c.tools.kb.base_url = Some("http://127.0.0.1:1/kb".into()));
        let err = run(&KbUpdateTool, &ctx, json!({})).await.unwrap_err();
        assert!(err.to_string().contains("kb update"), "{err}");
    }
}
