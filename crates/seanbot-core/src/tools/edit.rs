use std::borrow::Cow;
use std::path::Path;

use async_trait::async_trait;
use seanbot_provider::ToolSpec;
use serde_json::{Value, json};
use similar::{ChangeTag, TextDiff};

use crate::tool::{
    FileSnapshot, Risk, Tool, ToolContext, ToolError, ToolOutput, io_error, opt_bool, opt_u64,
    resolve_path, str_arg,
};

pub struct EditTool;

#[async_trait]
impl Tool for EditTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "edit".into(),
            description: "精确替换文件中的文本。old_string 必须与文件内容逐字一致（含缩进与空白）；同一段文本出现多次时用 occurrence 指定替换第几处，或用 replace_all 全部替换。修改已有文件前必须先用 read 读取；文件内容没变时读一次即可连续编辑多次。old_string 为空字符串且文件不存在时新建文件（自动创建父目录），内容为 new_string。唯一性失败会列出每一处出现的行号；把 read 输出的行号前缀一起复制进来时会被自动忽略并在结果中说明。配置文件 config.toml 受保护，不能通过本工具修改（请用 config 工具或手动编辑）。".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "文件路径"},
                    "old_string": {"type": "string", "description": "要被替换的原文；新建文件时传空字符串"},
                    "new_string": {"type": "string", "description": "替换后的内容"},
                    "replace_all": {"type": "boolean", "description": "替换所有出现处，默认 false"},
                    "occurrence": {"type": "integer", "description": "替换第几处出现（从 1 开始）；同一段文本出现多次时用它精确指定，不能与 replace_all 同时使用"}
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

    async fn preview(&self, args: &Value, ctx: &ToolContext) -> Option<String> {
        let raw = args.get("path")?.as_str()?;
        if ctx.is_config_file(&resolve_path(&ctx.cwd, raw)) {
            return Some(
                "（config.toml 是受保护的配置文件，不能修改；请用 config 工具或手动编辑）".into(),
            );
        }
        if ctx.is_builtin_kb(&resolve_path(&ctx.cwd, raw)) {
            return Some(
                "（内置知识库是官方内容，不能修改；要补自己的内容用 kb_add / kb_edit）".into(),
            );
        }
        let old = args.get("old_string")?.as_str()?;
        let new = args.get("new_string")?.as_str()?;
        let replace_all = args
            .get("replace_all")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let occurrence = match occurrence_arg(args) {
            Ok(value) => value,
            Err(e) => return Some(format!("（无法预览：{e}）")),
        };
        if old.is_empty() {
            return Some(limit_preview(
                new.lines().map(|l| format!("+ {l}")).collect(),
            ));
        }
        let path = resolve_path(&ctx.cwd, raw);
        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => return Some(format!("（无法预览：{}）", io_error(raw, e))),
        };
        match plan_edit(raw, &content, old, new, replace_all, occurrence) {
            Ok(plan) => Some(limit_preview(diff_stats(&content, &plan.updated).2)),
            Err(reason) => Some(format!("（无法预览：{reason}）")),
        }
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let raw = str_arg(&args, "path")?;
        let old = str_arg(&args, "old_string")?;
        let new = str_arg(&args, "new_string")?;
        let replace_all = opt_bool(&args, "replace_all")?.unwrap_or(false);
        let occurrence = occurrence_arg(&args)?;
        if occurrence.is_some() && replace_all {
            return Err(ToolError::InvalidArgs(
                "occurrence 与 replace_all 不能同时使用".into(),
            ));
        }
        let path = resolve_path(&ctx.cwd, raw);
        // 配置文件受保护：内置工具不能改，只能用 config 工具或手动编辑
        if ctx.is_config_file(&path) {
            return Err(ctx.config_file_error("edit 不能修改它"));
        }
        // 内置知识库同样受保护：官方内容只由 kb_update 更新
        if ctx.is_builtin_kb(&path) {
            return Err(ctx.builtin_kb_error("edit 不能修改它"));
        }

        if old.is_empty() {
            return create_file(raw, &path, new, ctx).await;
        }
        if old == new {
            return Err(ToolError::InvalidArgs(
                "old_string 与 new_string 相同，无需修改".into(),
            ));
        }

        let bytes = tokio::fs::read(&path).await.map_err(|e| io_error(raw, e))?;
        // 只看内容有没有变：mtime 被编辑器保存或上一次 edit 改动过，不代表内容过期
        let snapshot = FileSnapshot::of(&bytes);
        match ctx.reads.get(&path) {
            None => return Err(ToolError::Failed(format!("修改前请先用 read 读取 {raw}"))),
            Some(recorded) if recorded != snapshot => {
                return Err(ToolError::Failed(format!(
                    "{raw} 在 read 之后内容已被修改（读时 {} 字节，现在 {} 字节），请重新 read 后再编辑",
                    recorded.len, snapshot.len
                )));
            }
            Some(_) => {}
        }
        let Ok(source) = String::from_utf8(bytes) else {
            return Err(ToolError::Failed(format!(
                "{raw} 不是 UTF-8 文本，无法编辑"
            )));
        };

        let plan = plan_edit(raw, &source, old, new, replace_all, occurrence)
            .map_err(ToolError::Failed)?;
        tokio::fs::write(&path, &plan.updated)
            .await
            .map_err(|e| io_error(raw, e))?;
        ctx.reads
            .record(&path, FileSnapshot::of(plan.updated.as_bytes()));

        let (added, removed, preview) = diff_stats(&source, &plan.updated);
        let mut message = format!(
            "已修改 {raw}（替换 {} 处，+{added} -{removed} 行）",
            plan.replaced
        );
        for note in &plan.notes {
            message.push_str(&format!("\n提示：{note}"));
        }
        // 编辑子目录里的文件时，把它所在目录链上的指令文件注入一次
        if let Some(extra) =
            crate::instruction::render_block(&ctx.instructions.take_for(&path, &ctx.cwd))
        {
            message.push_str(&extra);
        }
        Ok(ToolOutput {
            content: message,
            summary: format!("+{added} -{removed} 行"),
            preview,
            is_error: false,
        })
    }
}

/// 一次替换的完整方案。与 `call`、`preview` 共用，保证预览与实际一致。
///
/// 对 `kb_edit` 可见：知识库条目的替换走同一套引擎，语义（精确优先、occurrence、诊断）完全一致。
pub(crate) struct EditPlan {
    pub(crate) updated: String,
    pub(crate) replaced: usize,
    /// 为了让匹配成功而做的自动调整，会回告给模型
    pub(crate) notes: Vec<String>,
}

/// 计算替换结果。匹配是"精确优先"的：只有精确匹配失败时才尝试剥离行号前缀，
/// 绝不引入模糊匹配，避免改到不是模型想改的那一处。
pub(crate) fn plan_edit(
    raw: &str,
    content: &str,
    old: &str,
    new: &str,
    replace_all: bool,
    occurrence: Option<usize>,
) -> Result<EditPlan, String> {
    let mut notes = Vec::new();
    if occurrence.is_some() && replace_all {
        return Err("occurrence 与 replace_all 不能同时使用".into());
    }

    // read 显示时去掉了 \r，模型给出的通常是 \n；两个方向都对齐到文件的实际换行符
    let (old_eol, new_eol, eol_note) = align_line_endings(content, old, new);
    let (mut old_text, mut new_text) = (old_eol.into_owned(), new_eol.into_owned());
    if let Some(note) = eol_note {
        notes.push(note);
    }

    let mut count = content.matches(old_text.as_str()).count();
    if count == 0
        && let Some(stripped) = strip_read_prefix(&old_text)
        && content.matches(stripped.as_str()).count() > 0
    {
        let stripped_new = strip_read_prefix(&new_text).unwrap_or_else(|| new_text.clone());
        count = content.matches(stripped.as_str()).count();
        old_text = stripped;
        new_text = stripped_new;
        notes.push("已忽略复制进来的 read 行号前缀（行号与 TAB 不属于文件内容）".into());
    }

    if count == 0 {
        return Err(not_found_message(raw));
    }
    if old_text == new_text {
        return Err("old_string 与 new_string 相同，无需修改".into());
    }

    // occurrence：直接改第 N 处，不必为了唯一性去猜更长的锚点
    if let Some(nth) = occurrence {
        if nth == 0 {
            return Err("occurrence 从 1 开始".into());
        }
        if nth > count {
            return Err(format!(
                "old_string 在 {raw} 中只出现 {count} 处（{}），无法替换第 {nth} 处",
                line_hint(&occurrence_lines(content, &old_text))
            ));
        }
        let (start, end) = nth_match_range(content, &old_text, nth).expect("已确认第 nth 处存在");
        let mut replaced_at = String::with_capacity(content.len() + new_text.len());
        replaced_at.push_str(&content[..start]);
        replaced_at.push_str(&new_text);
        replaced_at.push_str(&content[end..]);
        return Ok(EditPlan {
            updated: replaced_at,
            replaced: 1,
            notes,
        });
    }

    if count > 1 && !replace_all {
        return Err(ambiguous_message(raw, content, &old_text, count));
    }
    let updated = if replace_all {
        content.replace(old_text.as_str(), &new_text)
    } else {
        content.replacen(old_text.as_str(), &new_text, 1)
    };
    Ok(EditPlan {
        updated,
        replaced: if replace_all { count } else { 1 },
        notes,
    })
}

/// 把双方对齐到文件的换行风格，返回（对齐后的 old、对齐后的 new、说明）。
fn align_line_endings<'a>(
    content: &str,
    old: &'a str,
    new: &'a str,
) -> (Cow<'a, str>, Cow<'a, str>, Option<String>) {
    if content.contains("\r\n") && !old.contains('\r') {
        (
            Cow::Owned(old.replace('\n', "\r\n")),
            Cow::Owned(new.replace('\n', "\r\n")),
            Some("已按文件的 CRLF 换行符调整 old_string/new_string".into()),
        )
    } else if !content.contains("\r\n") && old.contains("\r\n") {
        (
            Cow::Owned(old.replace("\r\n", "\n")),
            Cow::Owned(new.replace("\r\n", "\n")),
            Some("已按文件的 LF 换行符调整 old_string/new_string".into()),
        )
    } else {
        (Cow::Borrowed(old), Cow::Borrowed(new), None)
    }
}

/// read 的输出每行是「行号（宽 6 右对齐）+ TAB + 内容」，模型常把行号一起复制进来，
/// 这样的文本在文件里必然找不到。
///
/// 只在每一行都符合 read 的行号格式、且行号连续时才剥离，返回去掉前缀的文本；
/// 返回 `None` 表示这段文本看起来不是从 read 输出里复制的。
fn strip_read_prefix(text: &str) -> Option<String> {
    let mut numbers: Vec<u64> = Vec::new();
    let mut bodies: Vec<String> = Vec::new();
    for line in text.split('\n') {
        if is_read_footer(line.trim()) {
            continue;
        }
        let (body, eol) = match line.strip_suffix('\r') {
            Some(body) => (body, "\r"),
            None => (line, ""),
        };
        if body.is_empty() {
            bodies.push(String::new());
            continue;
        }
        let (number, rest) = split_line_number(body)?;
        numbers.push(number);
        bodies.push(format!("{rest}{eol}"));
    }
    if numbers.is_empty() || numbers.windows(2).any(|pair| pair[1] != pair[0] + 1) {
        return None;
    }
    // read 的分段提示前会留一个空行
    while bodies.last().is_some_and(String::is_empty) {
        bodies.pop();
    }
    if bodies.is_empty() {
        return None;
    }
    Some(bodies.join("\n"))
}

/// 解析 `     12<TAB>` 前缀：数字补空格到宽 6（read 用 `{:>6}`），后跟一个 TAB。
fn split_line_number(line: &str) -> Option<(u64, &str)> {
    let spaces = line.len() - line.trim_start_matches(' ').len();
    let rest = &line[spaces..];
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || spaces + digits != 6 {
        return None;
    }
    let number = rest[..digits].parse().ok()?;
    let after = rest[digits..].strip_prefix('\t')?;
    Some((number, after))
}

/// read 分段读取时的提示行：`(文件共 10 行，本次显示第 3-4 行)`。
fn is_read_footer(line: &str) -> bool {
    let Some(inner) = line
        .strip_prefix("(文件共 ")
        .and_then(|rest| rest.strip_suffix(')'))
    else {
        return false;
    };
    let Some((total, tail)) = inner.split_once(" 行，本次显示第 ") else {
        return false;
    };
    let Some(range) = tail.strip_suffix(" 行") else {
        return false;
    };
    let Some((start, end)) = range.split_once('-') else {
        return false;
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    digits(total) && digits(start) && digits(end)
}

/// `occurrence` 参数：替换第几处出现（从 1 开始）。
fn occurrence_arg(args: &Value) -> Result<Option<usize>, ToolError> {
    match opt_u64(args, "occurrence")? {
        None => Ok(None),
        Some(0) => Err(ToolError::InvalidArgs("occurrence 从 1 开始".into())),
        Some(n) => usize::try_from(n)
            .map(Some)
            .map_err(|_| ToolError::InvalidArgs("occurrence 超出范围".into())),
    }
}

/// old_string 没找到时只回一句：细节诊断会让模型反复琢磨空白，不如直接重读。
fn not_found_message(raw: &str) -> String {
    format!("old_string 在 {raw} 中未找到；请重新 read 后按文件的实际内容重写（注意空白与换行）")
}

/// old_string 有多处匹配时列出行号，并给出两条出路：occurrence 指定第几处，或 replace_all。
fn ambiguous_message(raw: &str, content: &str, old: &str, count: usize) -> String {
    format!(
        "old_string 在 {raw} 中出现 {count} 次（{}）；请用 occurrence 指定第几处，或设置 replace_all=true 全部替换",
        line_hint(&occurrence_lines(content, old))
    )
}

/// 各处匹配的起始行号（从 1 开始）。
fn occurrence_lines(content: &str, old: &str) -> Vec<usize> {
    content
        .match_indices(old)
        .map(|(offset, _)| content[..offset].matches('\n').count() + 1)
        .collect()
}

/// 行号列表：`第 1、7、12 行`；超过 10 处只列前 10 个。
fn line_hint(lines: &[usize]) -> String {
    const SHOWN: usize = 10;
    let head: Vec<String> = lines.iter().take(SHOWN).map(usize::to_string).collect();
    if lines.len() > SHOWN {
        format!("第 {}、… 行（共 {} 处）", head.join("、"), lines.len())
    } else {
        format!("第 {} 行", head.join("、"))
    }
}

/// 第 n 处匹配的字节范围（n 从 1 开始）。
fn nth_match_range(content: &str, old: &str, nth: usize) -> Option<(usize, usize)> {
    content
        .match_indices(old)
        .nth(nth - 1)
        .map(|(start, matched)| (start, start + matched.len()))
}

const PREVIEW_LINES: usize = 20;

/// 把差异预览行限制在 20 行以内。
fn limit_preview(lines: Vec<String>) -> String {
    let total = lines.len();
    let mut shown: Vec<String> = lines.into_iter().take(PREVIEW_LINES).collect();
    if total > PREVIEW_LINES {
        shown.push(format!("…（共 {total} 行变更）"));
    }
    shown.join("\n")
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
    ctx.reads.record(path, FileSnapshot::of(content.as_bytes()));
    let lines = content.lines().count();
    let mut message = format!("已新建 {raw}（{lines} 行）");
    if let Some(extra) =
        crate::instruction::render_block(&ctx.instructions.take_for(path, &ctx.cwd))
    {
        message.push_str(&extra);
    }
    Ok(ToolOutput {
        content: message,
        summary: format!("新建文件 +{lines} 行"),
        preview: content.lines().map(|l| format!("+ {l}")).collect(),
        is_error: false,
    })
}

/// 返回（新增行数，删除行数，预览行）。
pub(crate) fn diff_stats(old: &str, new: &str) -> (usize, usize, Vec<String>) {
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

    async fn edit_error(ctx: &ToolContext, args: Value) -> String {
        EditTool.call(args, ctx).await.unwrap_err().to_string()
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
    async fn only_mtime_change_does_not_require_reread() {
        let (dir, ctx) = setup("hello\n");
        read(&ctx, "a.txt").await;
        // 编辑器保存、cargo fmt 之类只改 mtime：内容没变就不必重新 read
        fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
        let handle = fs::OpenOptions::new()
            .write(true)
            .open(dir.path().join("a.txt"))
            .unwrap();
        handle
            .set_modified(SystemTime::now() + Duration::from_secs(10))
            .unwrap();
        drop(handle);
        EditTool
            .call(
                json!({"path":"a.txt","old_string":"hello","new_string":"hi"}),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(file(&dir, "a.txt"), "hi\n");
    }

    #[tokio::test]
    async fn content_change_requires_reread() {
        let (dir, ctx) = setup("hello\n");
        read(&ctx, "a.txt").await;
        fs::write(dir.path().join("a.txt"), "hello world\n").unwrap();
        let err = edit_error(
            &ctx,
            json!({"path":"a.txt","old_string":"hello","new_string":"hi"}),
        )
        .await;
        assert!(err.contains("内容已被修改"), "{err}");
        assert!(err.contains("重新 read"), "{err}");

        read(&ctx, "a.txt").await;
        EditTool
            .call(
                json!({"path":"a.txt","old_string":"hello world","new_string":"hi"}),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(file(&dir, "a.txt"), "hi\n");
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
        let err = edit_error(
            &ctx,
            json!({"path":"a.txt","old_string":"foo","new_string":"bar"}),
        )
        .await;
        assert!(err.contains("出现 3 次"), "{err}");
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
    async fn ambiguous_match_lists_lines_and_suggests_occurrence() {
        let (_dir, ctx) = setup("foo\nbar\nfoo\n");
        read(&ctx, "a.txt").await;
        let err = edit_error(
            &ctx,
            json!({"path":"a.txt","old_string":"foo","new_string":"x"}),
        )
        .await;
        assert!(err.contains("出现 2 次"), "{err}");
        assert!(err.contains("第 1、3 行"), "{err}");
        assert!(err.contains("occurrence"), "{err}");
        assert!(err.contains("replace_all"), "{err}");
        assert_eq!(err.lines().count(), 1, "报错保持一行：{err}");
    }

    #[tokio::test]
    async fn not_found_and_identical_strings() {
        let (_dir, ctx) = setup("abc\n");
        read(&ctx, "a.txt").await;
        let err = edit_error(
            &ctx,
            json!({"path":"a.txt","old_string":"zzz","new_string":"y"}),
        )
        .await;
        assert!(err.contains("未找到"), "{err}");
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
    async fn ignores_line_number_prefix_copied_from_read() {
        let (dir, ctx) = setup("alpha\nbeta\ngamma\n");
        read(&ctx, "a.txt").await;
        let out = EditTool
            .call(
                json!({
                    "path": "a.txt",
                    "old_string": "     2\tbeta\n     3\tgamma\n",
                    "new_string": "     2\tB\n     3\tG\n"
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(file(&dir, "a.txt"), "alpha\nB\nG\n");
        assert!(out.content.contains("行号前缀"), "{}", out.content);
    }

    #[tokio::test]
    async fn strips_prefix_and_read_footer_together() {
        let (dir, ctx) = setup("a\nb\nc\nd\ne\n");
        ReadTool
            .call(json!({"path": "a.txt", "offset": 2, "limit": 2}), &ctx)
            .await
            .unwrap();
        let out = EditTool
            .call(
                json!({
                    "path": "a.txt",
                    "old_string": "     2\tb\n     3\tc\n\n(文件共 5 行，本次显示第 2-3 行)\n",
                    "new_string": "     2\tB\n     3\tC\n"
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(file(&dir, "a.txt"), "a\nB\nC\nd\ne\n");
        assert!(out.content.contains("行号前缀"), "{}", out.content);
    }

    #[tokio::test]
    async fn prefix_stripping_is_only_a_fallback() {
        let (dir, ctx) = setup("value\n     1\n");
        read(&ctx, "a.txt").await;
        // 文件里真的有 `     1` 这样一行：精确匹配优先，不会被当成行号剥掉
        let out = EditTool
            .call(
                json!({"path":"a.txt","old_string":"     1","new_string":"one"}),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(file(&dir, "a.txt"), "value\none\n");
        assert!(!out.content.contains("行号前缀"), "{}", out.content);
    }

    #[tokio::test]
    async fn missing_match_keeps_one_short_line() {
        let (_dir, ctx) = setup("a  \nb\n");
        read(&ctx, "a.txt").await;
        let err = edit_error(
            &ctx,
            json!({"path":"a.txt","old_string":"a\nb","new_string":"x"}),
        )
        .await;
        assert!(err.contains("未找到"), "{err}");
        assert!(err.contains("重新 read"), "{err}");
        assert!(!err.contains("行尾空白"), "不再做空白差异诊断：{err}");
        assert_eq!(err.lines().count(), 1, "报错保持一行：{err}");
    }

    #[tokio::test]
    async fn occurrence_replaces_the_requested_match() {
        let (dir, ctx) = setup("foo\nbar\nfoo\nbaz\nfoo\n");
        read(&ctx, "a.txt").await;
        let out = EditTool
            .call(
                json!({"path":"a.txt","old_string":"foo","new_string":"X","occurrence":2}),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(file(&dir, "a.txt"), "foo\nbar\nX\nbaz\nfoo\n");
        assert_eq!(out.summary, "+1 -1 行");
        // 读一次后可继续编辑；occurrence 每次都在当前内容上重新计数：
        // 第一处已改成 X，此时第 2 处就是原来的第 3 处
        EditTool
            .call(
                json!({"path":"a.txt","old_string":"foo","new_string":"Y","occurrence":2}),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(file(&dir, "a.txt"), "foo\nbar\nX\nbaz\nY\n");
    }

    #[tokio::test]
    async fn occurrence_skips_the_uniqueness_requirement() {
        let (dir, ctx) = setup("foo\nfoo\n");
        read(&ctx, "a.txt").await;
        EditTool
            .call(
                json!({"path":"a.txt","old_string":"foo","new_string":"1","occurrence":1}),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(file(&dir, "a.txt"), "1\nfoo\n");
    }

    #[tokio::test]
    async fn occurrence_out_of_range_reports_lines() {
        let (_dir, ctx) = setup("foo\nbar\nfoo\n");
        read(&ctx, "a.txt").await;
        let err = edit_error(
            &ctx,
            json!({"path":"a.txt","old_string":"foo","new_string":"x","occurrence":5}),
        )
        .await;
        assert!(err.contains("只出现 2 处"), "{err}");
        assert!(err.contains("第 1、3 行"), "{err}");
        assert!(err.contains("无法替换第 5 处"), "{err}");
        assert_eq!(err.lines().count(), 1, "报错保持一行：{err}");
    }

    #[tokio::test]
    async fn occurrence_rejects_zero_and_replace_all() {
        let (dir, ctx) = setup("foo\n");
        read(&ctx, "a.txt").await;
        let err = EditTool
            .call(
                json!({"path":"a.txt","old_string":"foo","new_string":"x","occurrence":0}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("occurrence 从 1 开始"), "{err}");
        let err = EditTool
            .call(
                json!({"path":"a.txt","old_string":"foo","new_string":"x","occurrence":1,"replace_all":true}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
        assert!(err.to_string().contains("不能同时使用"), "{err}");
        assert_eq!(file(&dir, "a.txt"), "foo\n");
    }

    #[tokio::test]
    async fn occurrence_preview_matches_actual_edit() {
        let (dir, ctx) = setup("foo\nbar\nfoo\n");
        let args = json!({"path":"a.txt","old_string":"foo","new_string":"X","occurrence":2});
        let preview = EditTool.preview(&args, &ctx).await.unwrap();
        assert_eq!(preview, "- foo\n+ X");
        read(&ctx, "a.txt").await;
        let out = EditTool.call(args, &ctx).await.unwrap();
        assert_eq!(out.preview.join("\n"), preview);
        assert_eq!(file(&dir, "a.txt"), "foo\nbar\nX\n");
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
        let out = EditTool
            .call(
                json!({"path":"a.txt","old_string":"a\nb","new_string":"x\ny"}),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(file(&dir, "a.txt"), "x\r\ny\r\nc\r\n");
        assert!(out.content.contains("CRLF"), "{}", out.content);
    }

    #[tokio::test]
    async fn lf_file_accepts_crlf_old_string() {
        let (dir, ctx) = setup("a\nb\n");
        read(&ctx, "a.txt").await;
        let out = EditTool
            .call(
                json!({"path":"a.txt","old_string":"a\r\nb","new_string":"x\ny"}),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(file(&dir, "a.txt"), "x\ny\n");
        assert!(out.content.contains("LF"), "{}", out.content);
    }

    #[tokio::test]
    async fn refuses_to_edit_config_file() {
        let (dir, mut ctx) = setup("");
        let path = dir.path().join("config.toml");
        fs::write(&path, "provider = \"deepseek\"\n").unwrap();
        ctx.config_path = Some(path.clone());
        let raw = path.to_string_lossy().to_string();
        let err = EditTool
            .call(
                json!({"path": raw, "old_string": "deepseek", "new_string": "other"}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("受保护的配置文件"), "{err}");
        assert_eq!(file(&dir, "config.toml"), "provider = \"deepseek\"\n");
        // 预览也会给出提示
        let preview = EditTool
            .preview(
                &json!({"path": raw, "old_string": "deepseek", "new_string": "other"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(preview.contains("受保护的配置文件"), "{preview}");
    }

    #[tokio::test]
    async fn preview_matches_edit_without_writing() {
        let (dir, ctx) = setup("a\nb\nc\n");
        let args = json!({"path":"a.txt","old_string":"b\n","new_string":"B1\nB2\n"});
        let preview = EditTool.preview(&args, &ctx).await.unwrap();
        assert_eq!(preview, "- b\n+ B1\n+ B2");
        assert_eq!(file(&dir, "a.txt"), "a\nb\nc\n", "预览不得修改文件");
        // 与真正执行时的预览行一致
        read(&ctx, "a.txt").await;
        let out = EditTool.call(args, &ctx).await.unwrap();
        assert_eq!(out.preview.join("\n"), preview);
    }

    #[tokio::test]
    async fn preview_of_prefixed_block_matches_actual_edit() {
        let (dir, ctx) = setup("alpha\nbeta\n");
        let args = json!({
            "path": "a.txt",
            "old_string": "     2\tbeta\n",
            "new_string": "     2\tB\n"
        });
        let preview = EditTool.preview(&args, &ctx).await.unwrap();
        assert_eq!(preview, "- beta\n+ B");
        read(&ctx, "a.txt").await;
        let out = EditTool.call(args, &ctx).await.unwrap();
        assert_eq!(out.preview.join("\n"), preview);
        assert_eq!(file(&dir, "a.txt"), "alpha\nB\n");
    }

    #[tokio::test]
    async fn preview_new_file_and_errors() {
        let (_dir, ctx) = setup("x\n");
        let p = EditTool
            .preview(
                &json!({"path":"n.txt","old_string":"","new_string":"1\n2\n"}),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(p, "+ 1\n+ 2");
        let p = EditTool
            .preview(
                &json!({"path":"a.txt","old_string":"zzz","new_string":"y"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(p.starts_with("（无法预览：") && p.contains("未找到"), "{p}");
        assert!(EditTool.preview(&json!({}), &ctx).await.is_none());
    }

    #[tokio::test]
    async fn preview_is_limited_to_20_lines() {
        let (_dir, ctx) = setup("");
        let body: String = (0..30).map(|i| format!("l{i}\n")).collect();
        let p = EditTool
            .preview(
                &json!({"path":"big.txt","old_string":"","new_string":body}),
                &ctx,
            )
            .await
            .unwrap();
        let lines: Vec<_> = p.lines().collect();
        assert_eq!(lines.len(), 21);
        assert_eq!(lines[20], "…（共 30 行变更）");
    }
}
