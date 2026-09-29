//! `bash_session` 工具：查看与关闭常驻 bash 会话。
//!
//! 会话由 `bash` 的 `session` 参数创建；这里只做管理：列出来、关掉、全关掉。
//! 程序退出时会自动全部清理（见 bash_session 模块），但用完主动关更省资源。

use async_trait::async_trait;
use seanbot_provider::ToolSpec;
use serde_json::{Value, json};

use crate::tool::{Risk, Tool, ToolContext, ToolError, ToolOutput, opt_str};

pub struct BashSessionTool;

#[async_trait]
impl Tool for BashSessionTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "bash_session".into(),
            description: "管理常驻 bash 会话（由 bash 的 session 参数创建）。action=list 列出当前活跃会话（名字、pid、当前目录、闲置时间）；action=close 关掉指定会话（需要 name）；action=close_all 全部关掉。会话用完请主动关：它们会一直占着进程，虽然程序退出时会全部清理。".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["list", "close", "close_all"], "description": "list 列出、close 关闭一个、close_all 全部关闭"},
                    "name": {"type": "string", "description": "action=close 时要关闭的会话名"}
                },
                "required": ["action"]
            }),
        }
    }

    fn risk(&self) -> Risk {
        Risk::Mutating
    }

    /// 只看不动不需要确认。
    fn read_only_call(&self, args: &Value) -> bool {
        matches!(
            args.get("action").and_then(Value::as_str),
            None | Some("list")
        )
    }

    fn title(&self, args: &Value) -> String {
        let action = args.get("action").and_then(Value::as_str).unwrap_or("list");
        match args.get("name").and_then(Value::as_str) {
            Some(name) if action == "close" => format!("关闭会话 {name}"),
            _ => format!("bash_session {action}"),
        }
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let action = opt_str(&args, "action")?
            .unwrap_or("list")
            .trim()
            .to_string();
        match action.as_str() {
            "list" => list(ctx),
            "close" => close(&args, ctx),
            "close_all" => close_all(ctx),
            other => Err(ToolError::InvalidArgs(format!(
                "action 只能是 list、close 或 close_all，收到「{other}」"
            ))),
        }
    }
}

fn list(ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
    let sessions = ctx.bash_sessions.list();
    let max = ctx.bash_sessions.max();
    if sessions.is_empty() {
        return Ok(ToolOutput::new(
            format!(
                "当前没有活跃的常驻会话（上限 {max} 个）。需要时用 bash 的 session 参数创建，例如 session=\"build\"。"
            ),
            "0 个会话",
        ));
    }
    let mut lines = vec![format!("活跃会话 {} 个（上限 {max}）:", sessions.len())];
    for item in &sessions {
        lines.push(format!(
            "- {} · pid {} · 当前目录 {}{} · 闲置 {} 秒 · 已存在 {} 秒",
            item.name,
            item.pid
                .map(|p| p.to_string())
                .unwrap_or_else(|| "?".into()),
            if item.cwd.is_empty() { "?" } else { &item.cwd },
            "",
            item.idle_secs,
            item.age_secs
        ));
    }
    lines.push("用完请 action=close 或 close_all；程序退出时也会全部清理。".to_string());
    let mut output = ToolOutput::new(lines.join("\n"), format!("{} 个会话", sessions.len()));
    output.preview = output.content.lines().take(3).map(String::from).collect();
    Ok(output)
}

fn close(args: &Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
    let name = opt_str(args, "name")?.unwrap_or("").trim().to_string();
    if name.is_empty() {
        return Err(ToolError::InvalidArgs(
            "action=close 需要提供 name（会话名）".into(),
        ));
    }
    let closed = ctx
        .bash_sessions
        .close(&name)
        .map_err(|e| ToolError::Failed(e.to_string()))?;
    Ok(ToolOutput::new(
        if closed {
            format!("已关闭会话 {name}（进程组已终止）")
        } else {
            format!("没有名为 {name} 的活跃会话（可能已经关了）")
        },
        if closed {
            format!("关闭会话 {name}")
        } else {
            format!("会话 {name} 不存在")
        },
    ))
}

fn close_all(ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
    let names = ctx.bash_sessions.close_all();
    if names.is_empty() {
        return Ok(ToolOutput::new("当前没有活跃会话，无需清理", "0 个会话"));
    }
    Ok(ToolOutput::new(
        format!(
            "已关闭 {} 个会话：{}（进程组均已终止）",
            names.len(),
            names.join("、")
        ),
        format!("关闭 {} 个会话", names.len()),
    ))
}
