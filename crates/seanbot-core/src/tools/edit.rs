use std::path::Path;

use async_trait::async_trait;
use seanbot_provider::ToolSpec;
use serde_json::{Value, json};
use similar::{ChangeTag, TextDiff};

use crate::tool::{
    Risk, Tool, ToolContext, ToolError, ToolOutput, io_error, opt_bool, resolve_path, str_arg,
};

pub struct EditTool;

#[async_trait]
impl Tool for EditTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "edit".into(),
            description: "精确替换文件中的文本。old_string 必须与文件内容逐字一致（含缩进与空白）且在文件中唯一出现；需要替换全部出现处时设置 replace_all。修改已有文件前必须先用 read 读取。old_string 为空字符串且文件不存在时新建文件（自动创建父目录），内容为 new_string。".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "文件路径"},
                    "old_string": {"type": "string", "description": "要被替换的原文；新建文件时传空字符串"},
                    "new_string": {"type": "string", "description": "替换后的内容"},
                    "replace_all": {"type": "boolean", "description": "替换所有出现处，默认 false"}
                },
                "required": ["path", "old_string", "new_string"]
            }),
        }
    }

    fn risk(&self) -> Risk {
        Risk::Mutating
    }

    fn title(&self, args: &Value) -> String {
        args.get("path")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let raw = str_arg(&args, "path")?;
        let old = str_arg(&args, "old_string")?;
        let new = str_arg(&args, "new_string")?;
        let replace_all = opt_bool(&args, "replace_all")?.unwrap_or(false);
        let path = resolve_path(&ctx.cwd, raw);

        if old.is_empty() {
            return create_file(raw, &path, new, ctx).await;
        }
        if old == new {
            return Err(ToolError::InvalidArgs(
                "old_string 与 new_string 相同，无需修改".into(),
            ));
        }

        let meta = tokio::fs::metadata(&path)
            .await
            .map_err(|e| io_error(raw, e))?;
        match ctx.reads.get(&path) {
            None => return Err(ToolError::Failed(format!("修改前请先用 read 读取 {raw}"))),
            Some(recorded) if meta.modified().ok() != Some(recorded) => {
                return Err(ToolError::Failed(format!(
                    "{raw} 在上次读取后已被修改，请重新 read 后再编辑"
                )));
            }
            Some(_) => {}
        }

        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| io_error(raw, e))?;
        // read 显示时去掉了 \r；CRLF 文件里把模型给出的 \n 还原成 \r\n
        let (old, new) = if content.contains("\r\n") && !old.contains('\r') {
            (old.replace('\n', "\r\n"), new.replace('\n', "\r\n"))
        } else {
            (old.to_string(), new.to_string())
        };
        let count = content.matches(old.as_str()).count();
        if count == 0 {
            return Err(ToolError::Failed(format!(
                "old_string 在 {raw} 中未找到；请确认内容（含缩进与空白）与文件完全一致"
            )));
        }
        if count > 1 && !replace_all {
            return Err(ToolError::Failed(format!(
                "old_string 在文件中出现 {count} 次，请提供更多上下文使其唯一，或设置 replace_all"
            )));
        }
        let updated = if replace_all {
            content.replace(old.as_str(), &new)
        } else {
            content.replacen(old.as_str(), &new, 1)
        };
        tokio::fs::write(&path, &updated)
            .await
            .map_err(|e| io_error(raw, e))?;
        record_mtime(ctx, &path).await;

        let replaced = if replace_all { count } else { 1 };
        let (added, removed, preview) = diff_stats(&content, &updated);
        Ok(ToolOutput {
            content: format!("已修改 {raw}（替换 {replaced} 处，+{added} -{removed} 行）"),
            summary: format!("+{added} -{removed} 行"),
            preview,
            is_error: false,
        })
    }
}

async fn create_file(
    raw: &str,
    path: &Path,
    content: &str,
    ctx: &ToolContext,
) -> Result<ToolOutput, ToolError> {
    if tokio::fs::try_exists(path).await.unwrap_or(false) {
        return Err(ToolError::Failed(format!(
            "{raw} 已存在；修改已有文件需提供非空的 old_string"
        )));
    }
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| io_error(raw, e))?;
    }
    tokio::fs::write(path, content)
        .await
        .map_err(|e| io_error(raw, e))?;
    record_mtime(ctx, path).await;
    let lines = content.lines().count();
    Ok(ToolOutput {
        content: format!("已新建 {raw}（{lines} 行）"),
        summary: format!("新建文件 +{lines} 行"),
        preview: content.lines().map(|l| format!("+ {l}")).collect(),
        is_error: false,
    })
}

/// 写入后刷新读取记录，允许连续编辑。
async fn record_mtime(ctx: &ToolContext, path: &Path) {
    if let Ok(mtime) = tokio::fs::metadata(path).await.and_then(|m| m.modified()) {
        ctx.reads.record(path, mtime);
    }
}

/// 返回（新增行数，删除行数，预览行）。
fn diff_stats(old: &str, new: &str) -> (usize, usize, Vec<String>) {
    let diff = TextDiff::from_lines(old, new);
    let (mut added, mut removed, mut preview) = (0, 0, Vec::new());
    for change in diff.iter_all_changes() {
        let text = change.value().trim_end_matches(['\n', '\r']);
        match change.tag() {
            ChangeTag::Insert => {
                added += 1;
                preview.push(format!("+ {text}"));
            }
            ChangeTag::Delete => {
                removed += 1;
                preview.push(format!("- {text}"));
            }
            ChangeTag::Equal => {}
        }
    }
    (added, removed, preview)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ReadTool;
    use std::{
        fs,
        time::{Duration, SystemTime},
    };

    fn setup(content: &str) -> (tempfile::TempDir, ToolContext) {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), content).unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        (dir, ctx)
    }

    async fn read(ctx: &ToolContext, path: &str) {
        ReadTool.call(json!({"path": path}), ctx).await.unwrap();
    }

    fn file(dir: &tempfile::TempDir, name: &str) -> String {
        fs::read_to_string(dir.path().join(name)).unwrap()
    }

    #[tokio::test]
    async fn requires_prior_read() {
        let (_dir, ctx) = setup("hello\n");
        let err = EditTool
            .call(
                json!({"path":"a.txt","old_string":"hello","new_string":"hi"}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("请先用 read 读取"));
    }

    #[tokio::test]
    async fn rejects_when_modified_after_read() {
        let (dir, ctx) = setup("hello\n");
        read(&ctx, "a.txt").await;
        let f = fs::OpenOptions::new()
            .write(true)
            .open(dir.path().join("a.txt"))
            .unwrap();
        f.set_modified(SystemTime::now() + Duration::from_secs(10))
            .unwrap();
        let err = EditTool
            .call(
                json!({"path":"a.txt","old_string":"hello","new_string":"hi"}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("已被修改"));
    }

    #[tokio::test]
    async fn replaces_unique_match_and_reports_diff() {
        let (dir, ctx) = setup("a\nb\nc\n");
        read(&ctx, "a.txt").await;
        let out = EditTool
            .call(
                json!({"path":"a.txt","old_string":"b\n","new_string":"B1\nB2\n"}),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(file(&dir, "a.txt"), "a\nB1\nB2\nc\n");
        assert_eq!(out.summary, "+2 -1 行");
        assert_eq!(out.preview, vec!["- b", "+ B1", "+ B2"]);
        assert!(!out.is_error);
    }

    #[tokio::test]
    async fn consecutive_edits_after_single_read() {
        let (dir, ctx) = setup("x y\n");
        read(&ctx, "a.txt").await;
        EditTool
            .call(
                json!({"path":"a.txt","old_string":"x","new_string":"1"}),
                &ctx,
            )
            .await
            .unwrap();
        EditTool
            .call(
                json!({"path":"a.txt","old_string":"y","new_string":"2"}),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(file(&dir, "a.txt"), "1 2\n");
    }

    #[tokio::test]
    async fn multiple_matches_need_replace_all() {
        let (dir, ctx) = setup("foo foo foo\n");
        read(&ctx, "a.txt").await;
        let err = EditTool
            .call(
                json!({"path":"a.txt","old_string":"foo","new_string":"bar"}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("出现 3 次"));
        EditTool
            .call(
                json!({"path":"a.txt","old_string":"foo","new_string":"bar","replace_all":true}),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(file(&dir, "a.txt"), "bar bar bar\n");
    }

    #[tokio::test]
    async fn not_found_and_identical_strings() {
        let (_dir, ctx) = setup("abc\n");
        read(&ctx, "a.txt").await;
        let err = EditTool
            .call(
                json!({"path":"a.txt","old_string":"zzz","new_string":"y"}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("未找到"));
        let err = EditTool
            .call(
                json!({"path":"a.txt","old_string":"abc","new_string":"abc"}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("相同"));
    }

    #[tokio::test]
    async fn creates_new_file_with_parents() {
        let (dir, ctx) = setup("");
        let out = EditTool
            .call(
                json!({"path":"deep/new/b.txt","old_string":"","new_string":"one\ntwo\n"}),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(file(&dir, "deep/new/b.txt"), "one\ntwo\n");
        assert_eq!(out.summary, "新建文件 +2 行");
        // 新建后可以直接继续编辑
        EditTool
            .call(
                json!({"path":"deep/new/b.txt","old_string":"two","new_string":"2"}),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(file(&dir, "deep/new/b.txt"), "one\n2\n");
    }

    #[tokio::test]
    async fn empty_old_string_on_existing_file_is_error() {
        let (_dir, ctx) = setup("x");
        let err = EditTool
            .call(
                json!({"path":"a.txt","old_string":"","new_string":"y"}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("已存在"));
    }

    #[tokio::test]
    async fn edit_matches_crlf_file() {
        let (dir, ctx) = setup("a\r\nb\r\nc\r\n");
        read(&ctx, "a.txt").await;
        EditTool
            .call(
                json!({"path":"a.txt","old_string":"a\nb","new_string":"x\ny"}),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(file(&dir, "a.txt"), "x\r\ny\r\nc\r\n");
    }
}
