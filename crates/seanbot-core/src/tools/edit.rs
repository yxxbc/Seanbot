use std::borrow::Cow;
use std::path::Path;

use async_trait::async_trait;
use seanbot_provider::ToolSpec;
use serde_json::{Value, json};
use similar::{ChangeTag, TextDiff};

use crate::tool::{
    FileSnapshot, Risk, Tool, ToolContext, ToolError, ToolOutput, io_error, opt_bool, resolve_path,
    str_arg,
};

pub struct EditTool;

#[async_trait]
impl Tool for EditTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "edit".into(),
            description: "精确替换文件中的文本。old_string 必须与文件内容逐字一致（含缩进与空白）且在文件中唯一出现；需要替换全部出现处时设置 replace_all。修改已有文件前必须先用 read 读取；文件内容没变时读一次即可连续编辑多次。old_string 为空字符串且文件不存在时新建文件（自动创建父目录），内容为 new_string。唯一性失败会列出每一处出现的行号；把 read 输出的行号前缀一起复制进来时会被自动忽略并在结果中说明。".into(),
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

    async fn preview(&self, args: &Value, ctx: &ToolContext) -> Option<String> {
        let raw = args.get("path")?.as_str()?;
        let old = args.get("old_string")?.as_str()?;
        let new = args.get("new_string")?.as_str()?;
        let replace_all = args
            .get("replace_all")
            .and_then(Value::as_bool)
            .unwrap_or(false);
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
        match plan_edit(raw, &content, old, new, replace_all) {
            Ok(plan) => Some(limit_preview(diff_stats(&content, &plan.updated).2)),
            Err(reason) => Some(format!("（无法预览：{reason}）")),
        }
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

        let plan = plan_edit(raw, &source, old, new, replace_all).map_err(ToolError::Failed)?;
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
        Ok(ToolOutput {
            content: message,
            summary: format!("+{added} -{removed} 行"),
            preview,
            is_error: false,
        })
    }
}

/// 一次替换的完整方案。与 `call`、`preview` 共用，保证预览与实际一致。
struct EditPlan {
    updated: String,
    replaced: usize,
    /// 为了让匹配成功而做的自动调整，会回告给模型
    notes: Vec<String>,
}

/// 计算替换结果。匹配是"精确优先"的：只有精确匹配失败时才尝试剥离行号前缀，
/// 绝不引入模糊匹配，避免改到不是模型想改的那一处。
fn plan_edit(
    raw: &str,
    content: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<EditPlan, String> {
    let mut notes = Vec::new();

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
        return Err(attach_notes(
            not_found_message(raw, content, &old_text),
            &notes,
        ));
    }
    if old_text == new_text {
        return Err("old_string 与 new_string 相同，无需修改".into());
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

/// 逐行比较用的归一化函数。
type LineNorm = fn(&str) -> String;

/// old_string 没找到时，尽量说清"差在哪"，而不是只回一句未找到。
fn not_found_message(raw: &str, content: &str, old: &str) -> String {
    let variants: [(&str, LineNorm); 4] = [
        ("行尾空白不同", |line: &str| {
            line.trim_end().to_string()
        }),
        ("缩进宽度不同", normalize_indent),
        ("空白差异（空格、TAB 或全角空格）", squash_whitespace),
        ("换行符不同", |line: &str| line.replace('\r', "")),
    ];
    for (label, norm) in variants {
        if let Some((start, len)) = locate(content, old, norm) {
            let span = if len == 1 {
                format!("第 {start} 行")
            } else {
                format!("第 {start}-{} 行", start + len - 1)
            };
            let mut message =
                format!("old_string 在 {raw} 中未找到：{span}的内容与它一致，但{label}。");
            let sample = sample_difference(content, old, start);
            if !sample.is_empty() {
                message.push('\n');
                message.push_str(&sample);
            }
            message.push_str("\n请重新 read 后按文件里的实际内容重写 old_string。");
            return message;
        }
    }

    // 整段对不上时退一步：只报告首行在哪
    if let Some(first) = old.lines().find(|line| !line.trim().is_empty())
        && let Some(line) = find_line(content, first)
    {
        return format!(
            "old_string 在 {raw} 中未找到；它的首行 `{}` 出现在第 {line} 行，但整段与文件不符。\n请重新 read 后按文件的实际内容重写 old_string（不要带行号前缀）。",
            clip(first.trim())
        );
    }
    format!(
        "old_string 在 {raw} 中未找到；请重新 read {raw} 并确认内容（含缩进与空白）与文件完全一致"
    )
}

/// old_string 有多处匹配时，列出每一处的行号与所在行，方便模型判断要补多少上下文。
fn ambiguous_message(raw: &str, content: &str, old: &str, count: usize) -> String {
    const SHOWN: usize = 5;
    let mut message = format!("old_string 在 {raw} 中出现 {count} 次，无法确定改哪一处：");
    for (line, text) in occurrences(content, old).into_iter().take(SHOWN) {
        message.push_str(&format!("\n  第 {line} 行：{}", visible(&clip(&text))));
    }
    if count > SHOWN {
        message.push_str(&format!("\n  …共 {count} 处"));
    }
    message
        .push_str("\n请把 old_string 扩展到包含相邻行使其唯一，或设置 replace_all=true 替换全部");
    message
}

/// 每次匹配的（起始行号，该行内容）。
fn occurrences(content: &str, old: &str) -> Vec<(usize, String)> {
    content
        .match_indices(old)
        .map(|(offset, _)| {
            let line = content[..offset].matches('\n').count() + 1;
            let text = content[offset..].lines().next().unwrap_or_default();
            (line, text.to_string())
        })
        .collect()
}

/// 按归一化后的逐行比较，找 old 出现在文件的哪几行；返回（起始行号，行数）。
fn locate(content: &str, old: &str, norm: LineNorm) -> Option<(usize, usize)> {
    let hay: Vec<String> = content.lines().map(norm).collect();
    let needle: Vec<String> = old.lines().map(norm).collect();
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len())
        .find(|&i| hay[i..i + needle.len()] == needle[..])
        .map(|i| (i + 1, needle.len()))
}

/// 第一处不一致的样子，空白用 `·`（空格）与 `→`（TAB）显形。
///
/// 比较的是原始行：能走到这里说明归一化后双方一致，差异只可能出在空白上。
fn sample_difference(content: &str, old: &str, start: usize) -> String {
    let hay: Vec<&str> = content.lines().collect();
    for (i, expected) in old.lines().enumerate() {
        let Some(actual) = hay.get(start - 1 + i) else {
            break;
        };
        // 比原始行：归一化后的内容按定义是相同的，看不出差在哪一行
        if *actual != expected {
            return format!(
                "第 {} 行：文件是 `{}`，old_string 是 `{}`",
                start + i,
                visible(&clip(actual)),
                visible(&clip(expected))
            );
        }
    }
    String::new()
}

/// 某一行（忽略行尾空白）在文件中的行号。
fn find_line(content: &str, needle: &str) -> Option<usize> {
    content
        .lines()
        .position(|line| line == needle || line.trim_end() == needle.trim_end())
        .map(|i| i + 1)
}

/// 全角空格等 Unicode 空白统一成半角，再去掉首尾空白、把连续空白压成一个空格。
fn squash_whitespace(line: &str) -> String {
    let mut out = String::new();
    let mut pending_space = false;
    for ch in line.chars() {
        if ch.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(ch);
    }
    out
}

/// 只比较缩进宽度：把每一行的前导空白压成一个 TAB，忽略正文里的空白差异。
fn normalize_indent(line: &str) -> String {
    let indent = line.len() - line.trim_start().len();
    format!("\t{}", &line[indent..])
}

/// 把空白显形，避免模型看不出"看起来一样"的两行差在哪。
fn visible(line: &str) -> String {
    line.chars()
        .map(|c| match c {
            ' ' => '·',
            '\t' => '→',
            other => other,
        })
        .collect()
}

/// 截到便于放进错误信息的长度。
fn clip(text: &str) -> String {
    const MAX: usize = 60;
    if text.chars().count() <= MAX {
        return text.to_string();
    }
    let mut out: String = text.chars().take(MAX).collect();
    out.push('…');
    out
}

fn attach_notes(mut message: String, notes: &[String]) -> String {
    for note in notes {
        message.push_str(&format!("\n（{note}）"));
    }
    message
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
    Ok(ToolOutput {
        content: format!("已新建 {raw}（{lines} 行）"),
        summary: format!("新建文件 +{lines} 行"),
        preview: content.lines().map(|l| format!("+ {l}")).collect(),
        is_error: false,
    })
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
    async fn ambiguous_match_lists_every_occurrence() {
        let (_dir, ctx) = setup("foo\nbar\nfoo\n");
        read(&ctx, "a.txt").await;
        let err = edit_error(
            &ctx,
            json!({"path":"a.txt","old_string":"foo","new_string":"x"}),
        )
        .await;
        assert!(err.contains("出现 2 次"), "{err}");
        assert!(err.contains("第 1 行"), "{err}");
        assert!(err.contains("第 3 行"), "{err}");
        assert!(err.contains("replace_all"), "{err}");
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
    async fn missing_match_reports_trailing_whitespace() {
        let (_dir, ctx) = setup("a  \nb\n");
        read(&ctx, "a.txt").await;
        let err = edit_error(
            &ctx,
            json!({"path":"a.txt","old_string":"a\nb","new_string":"x"}),
        )
        .await;
        assert!(err.contains("第 1-2 行"), "{err}");
        assert!(err.contains("行尾空白"), "{err}");
        assert!(err.contains("a··"), "应把空格显形：{err}");
    }

    #[tokio::test]
    async fn missing_match_reports_full_width_space() {
        let (_dir, ctx) = setup("a\u{3000}b\n");
        read(&ctx, "a.txt").await;
        let err = edit_error(
            &ctx,
            json!({"path":"a.txt","old_string":"a b","new_string":"x"}),
        )
        .await;
        assert!(err.contains("第 1 行"), "{err}");
        assert!(err.contains("空白"), "{err}");
    }

    #[tokio::test]
    async fn missing_match_reports_indent_difference() {
        let (_dir, ctx) = setup("if x {\n\tbody\n}\n");
        read(&ctx, "a.txt").await;
        let err = edit_error(
            &ctx,
            json!({"path":"a.txt","old_string":"if x {\n    body\n}","new_string":"y"}),
        )
        .await;
        assert!(err.contains("缩进"), "{err}");
    }

    #[tokio::test]
    async fn missing_match_falls_back_to_first_line_position() {
        let (_dir, ctx) = setup("alpha\nbeta\n");
        read(&ctx, "a.txt").await;
        let err = edit_error(
            &ctx,
            json!({"path":"a.txt","old_string":"beta\ngamma","new_string":"x"}),
        )
        .await;
        assert!(err.contains("首行 `beta` 出现在第 2 行"), "{err}");
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
