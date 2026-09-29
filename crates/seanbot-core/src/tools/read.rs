use async_trait::async_trait;
use seanbot_provider::ToolSpec;
use serde_json::{Value, json};

use crate::tool::{
    Risk, Tool, ToolContext, ToolError, ToolOutput, io_error, is_binary, opt_u64, resolve_path,
    str_arg,
};

const DEFAULT_LIMIT: u64 = 2000;
const MAX_LINE_CHARS: usize = 2000;

pub struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read".into(),
            description: "读取文本文件，返回带行号的内容（每行格式：行号<TAB>内容）。默认从第 1 行起最多读取 2000 行，可用 offset（起始行号，从 1 开始）和 limit 分段读取。修改已有文件前必须先用本工具读取。".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "文件路径，相对路径以工作目录为基准"},
                    "offset": {"type": "integer", "description": "起始行号，从 1 开始"},
                    "limit": {"type": "integer", "description": "最多读取的行数"}
                },
                "required": ["path"]
            }),
        }
    }

    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }

    fn title(&self, args: &Value) -> String {
        args.get("path")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let raw = str_arg(&args, "path")?;
        let path = resolve_path(&ctx.cwd, raw);
        let offset = opt_u64(&args, "offset")?.unwrap_or(1).max(1) as usize;
        let limit = opt_u64(&args, "limit")?.unwrap_or(DEFAULT_LIMIT).max(1) as usize;

        let meta = tokio::fs::metadata(&path)
            .await
            .map_err(|e| io_error(raw, e))?;
        if meta.is_dir() {
            return Err(ToolError::Failed(format!(
                "{raw} 是目录，不是文件；列出目录内容请用 search 的 glob 参数"
            )));
        }
        let bytes = tokio::fs::read(&path).await.map_err(|e| io_error(raw, e))?;
        if is_binary(&bytes) {
            return Err(ToolError::Failed(format!("{raw} 是二进制文件，无法读取")));
        }
        if let Ok(mtime) = meta.modified() {
            ctx.reads.record(&path, mtime);
        }

        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text.lines().collect();
        let total = lines.len();
        if total == 0 {
            return Ok(ToolOutput::new("(空文件)", "空文件"));
        }
        if offset > total {
            return Err(ToolError::Failed(format!(
                "offset {offset} 超出文件总行数 {total}"
            )));
        }
        let end = (offset - 1 + limit).min(total);
        let mut out = String::new();
        for (i, line) in lines[offset - 1..end].iter().enumerate() {
            out.push_str(&format!("{:>6}\t{}\n", offset + i, truncate_line(line)));
        }
        if offset > 1 || end < total {
            out.push_str(&format!(
                "\n(文件共 {total} 行，本次显示第 {offset}-{end} 行)\n"
            ));
        }
        Ok(ToolOutput::new(
            out,
            format!("读取 {} 行", end - offset + 1),
        ))
    }
}

fn truncate_line(line: &str) -> String {
    if line.chars().count() <= MAX_LINE_CHARS {
        return line.to_string();
    }
    let head: String = line.chars().take(MAX_LINE_CHARS).collect();
    format!("{head}…[本行过长已截断]")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup() -> (tempfile::TempDir, ToolContext) {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        (dir, ctx)
    }

    #[tokio::test]
    async fn numbered_lines_and_tracks_read() {
        let (dir, ctx) = setup();
        fs::write(dir.path().join("a.txt"), "alpha\nbeta\n").unwrap();
        let out = ReadTool.call(json!({"path": "a.txt"}), &ctx).await.unwrap();
        assert_eq!(out.content, "     1\talpha\n     2\tbeta\n");
        assert_eq!(out.summary, "读取 2 行");
        assert!(ctx.reads.get(&dir.path().join("a.txt")).is_some());
    }

    #[tokio::test]
    async fn offset_and_limit() {
        let (dir, ctx) = setup();
        let text: String = (1..=10).map(|i| format!("line{i}\n")).collect();
        fs::write(dir.path().join("a.txt"), text).unwrap();
        let out = ReadTool
            .call(json!({"path": "a.txt", "offset": 3, "limit": 2}), &ctx)
            .await
            .unwrap();
        assert!(out.content.starts_with("     3\tline3\n     4\tline4\n"));
        assert!(out.content.contains("文件共 10 行，本次显示第 3-4 行"));
    }

    #[tokio::test]
    async fn offset_past_end_is_error() {
        let (dir, ctx) = setup();
        fs::write(dir.path().join("a.txt"), "x\n").unwrap();
        let err = ReadTool
            .call(json!({"path": "a.txt", "offset": 5}), &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("超出文件总行数 1"));
    }

    #[tokio::test]
    async fn long_lines_truncated() {
        let (dir, ctx) = setup();
        fs::write(dir.path().join("a.txt"), "字".repeat(2500)).unwrap();
        let out = ReadTool.call(json!({"path": "a.txt"}), &ctx).await.unwrap();
        assert!(out.content.contains("…[本行过长已截断]"));
        assert_eq!(out.content.matches('字').count(), 2000);
    }

    #[tokio::test]
    async fn binary_missing_and_directory_are_errors() {
        let (dir, ctx) = setup();
        fs::write(dir.path().join("b.bin"), [0u8, 1, 2]).unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        let bin = ReadTool
            .call(json!({"path": "b.bin"}), &ctx)
            .await
            .unwrap_err();
        assert!(bin.to_string().contains("二进制"));
        let missing = ReadTool
            .call(json!({"path": "nope.txt"}), &ctx)
            .await
            .unwrap_err();
        assert_eq!(missing.to_string(), "文件不存在：nope.txt");
        let is_dir = ReadTool
            .call(json!({"path": "sub"}), &ctx)
            .await
            .unwrap_err();
        assert!(is_dir.to_string().contains("是目录"));
    }

    #[tokio::test]
    async fn empty_file() {
        let (dir, ctx) = setup();
        fs::write(dir.path().join("e.txt"), "").unwrap();
        let out = ReadTool.call(json!({"path": "e.txt"}), &ctx).await.unwrap();
        assert_eq!(out.content, "(空文件)");
        assert!(ctx.reads.get(&dir.path().join("e.txt")).is_some());
    }

    #[tokio::test]
    async fn missing_path_arg() {
        let (_dir, ctx) = setup();
        let err = ReadTool.call(json!({}), &ctx).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }
}
