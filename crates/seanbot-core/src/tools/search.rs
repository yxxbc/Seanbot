use std::path::{Path, PathBuf};

use async_trait::async_trait;
use globset::{Glob, GlobMatcher};
use ignore::WalkBuilder;
use regex::Regex;
use seanbot_provider::ToolSpec;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::tool::{
    Risk, Tool, ToolContext, ToolError, ToolOutput, is_binary, opt_str, opt_u64, resolve_path,
};

const DEFAULT_MAX: u64 = 200;
const MAX_LINE_CHARS: usize = 300;

pub struct SearchTool;

#[async_trait]
impl Tool for SearchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "search".into(),
            description: "在工作目录下搜索文件与代码，遵守 .gitignore。只给 glob 时按路径模式列出文件（如 \"*.rs\"、\"src/**/*.toml\"）；给 pattern 时用正则搜索文件内容，输出 路径:行号: 内容，可再用 glob 限定文件范围。pattern 与 glob 至少提供一个。默认最多 200 条结果。".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "正则表达式，搜索文件内容"},
                    "glob": {"type": "string", "description": "文件路径 glob，相对搜索根目录"},
                    "path": {"type": "string", "description": "搜索根目录或单个文件，默认工作目录"},
                    "max_results": {"type": "integer", "description": "最多返回条数，默认 200"}
                }
            }),
        }
    }

    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }

    fn title(&self, args: &Value) -> String {
        args.get("pattern")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| args.get("glob").and_then(Value::as_str))
            .unwrap_or_default()
            .to_string()
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let pattern = opt_str(&args, "pattern")?.filter(|s| !s.is_empty());
        let glob = opt_str(&args, "glob")?.filter(|s| !s.is_empty());
        if pattern.is_none() && glob.is_none() {
            return Err(ToolError::InvalidArgs(
                "pattern 与 glob 至少提供一个".into(),
            ));
        }
        let regex = pattern
            .map(Regex::new)
            .transpose()
            .map_err(|e| ToolError::InvalidArgs(format!("正则表达式无效：{e}")))?;
        let matcher = glob
            .map(|g| Glob::new(g).map(|g| g.compile_matcher()))
            .transpose()
            .map_err(|e| ToolError::InvalidArgs(format!("glob 无效：{e}")))?;
        let raw_root = opt_str(&args, "path")?.unwrap_or(".");
        let root = if raw_root == "." {
            ctx.cwd.clone()
        } else {
            resolve_path(&ctx.cwd, raw_root)
        };
        if !root.exists() {
            return Err(ToolError::Failed(format!("路径不存在：{raw_root}")));
        }
        let max = opt_u64(&args, "max_results")?.unwrap_or(DEFAULT_MAX).max(1) as usize;

        let cwd = ctx.cwd.clone();
        let cancel = ctx.cancel.clone();
        let found = tokio::task::spawn_blocking(move || {
            run_search(&root, &cwd, matcher.as_ref(), regex.as_ref(), max, &cancel)
        })
        .await
        .map_err(|e| ToolError::Failed(format!("搜索任务异常：{e}")))??;

        let n = found.lines.len();
        let summary = if found.truncated {
            format!("{n} 条结果（已截断）")
        } else {
            format!("{n} 条结果")
        };
        let mut content = if n == 0 {
            "无匹配结果".to_string()
        } else {
            found.lines.join("\n")
        };
        if found.truncated {
            content.push_str(&format!("\n(结果超过 {max} 条，已截断；请缩小搜索范围)"));
        }
        Ok(ToolOutput {
            content,
            summary,
            preview: found.lines,
            is_error: false,
        })
    }
}

struct Found {
    lines: Vec<String>,
    truncated: bool,
}

fn run_search(
    root: &Path,
    cwd: &Path,
    glob: Option<&GlobMatcher>,
    regex: Option<&Regex>,
    max: usize,
    cancel: &CancellationToken,
) -> Result<Found, ToolError> {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .require_git(false)
        .sort_by_file_name(|a, b| a.cmp(b))
        .filter_entry(|e| e.file_name() != ".git");
    let mut lines = Vec::new();
    let mut truncated = false;
    'walk: for entry in builder.build() {
        if cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        if let Some(m) = glob {
            let rel = path.strip_prefix(root).unwrap_or(path);
            // 搜索根是单个文件时 strip 后为空，改用文件名匹配
            let target: PathBuf = if rel.as_os_str().is_empty() {
                path.file_name().map(PathBuf::from).unwrap_or_default()
            } else {
                rel.to_path_buf()
            };
            if !m.is_match(&target) {
                continue;
            }
        }
        let shown = display_path(path.strip_prefix(cwd).unwrap_or(path));
        match regex {
            None => {
                if lines.len() >= max {
                    truncated = true;
                    break;
                }
                lines.push(shown);
            }
            Some(re) => {
                let Ok(bytes) = std::fs::read(path) else {
                    continue;
                };
                if is_binary(&bytes) {
                    continue;
                }
                let text = String::from_utf8_lossy(&bytes);
                for (i, line) in text.lines().enumerate() {
                    if re.is_match(line) {
                        if lines.len() >= max {
                            truncated = true;
                            break 'walk;
                        }
                        lines.push(format!(
                            "{shown}:{}: {}",
                            i + 1,
                            clip(line.trim_end(), MAX_LINE_CHARS)
                        ));
                    }
                }
            }
        }
    }
    Ok(Found { lines, truncated })
}

/// 统一用 `/` 显示路径，Windows 上与其他平台保持一致。
fn display_path(p: &Path) -> String {
    let s = p.display().to_string();
    if cfg!(windows) {
        s.replace('\\', "/")
    } else {
        s
    }
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max).collect();
    t.push('…');
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup() -> (tempfile::TempDir, ToolContext) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src/nested")).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join(".config")).unwrap();
        fs::write(
            root.join("src/main.rs"),
            "fn main() {\n    println!(\"hi\");\n}\n",
        )
        .unwrap();
        fs::write(root.join("src/nested/lib.rs"), "pub fn helper() {}\n").unwrap();
        fs::write(root.join("README.md"), "# main project\n").unwrap();
        fs::write(root.join(".gitignore"), "ignored.rs\n").unwrap();
        fs::write(root.join("ignored.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join(".git/config"), "fn main\n").unwrap();
        fs::write(root.join(".config/tool.toml"), "main = true\n").unwrap();
        fs::write(root.join("data.bin"), b"main\0\0").unwrap();
        let ctx = ToolContext::new(root.to_path_buf());
        (dir, ctx)
    }

    async fn search(ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        SearchTool.call(args, ctx).await
    }

    #[tokio::test]
    async fn glob_lists_files_respecting_gitignore() {
        let (_d, ctx) = setup();
        let out = search(&ctx, json!({"glob": "*.rs"})).await.unwrap();
        assert_eq!(out.content, "src/main.rs\nsrc/nested/lib.rs");
        assert_eq!(out.summary, "2 条结果");
    }

    #[tokio::test]
    async fn pattern_searches_content() {
        let (_d, ctx) = setup();
        let out = search(&ctx, json!({"pattern": "fn \\w+", "glob": "*.rs"}))
            .await
            .unwrap();
        assert_eq!(
            out.content,
            "src/main.rs:1: fn main() {\nsrc/nested/lib.rs:1: pub fn helper() {}"
        );
    }

    #[tokio::test]
    async fn includes_hidden_files_but_not_git_dir_or_binaries() {
        let (_d, ctx) = setup();
        let out = search(&ctx, json!({"pattern": "main"})).await.unwrap();
        assert!(out.content.contains(".config/tool.toml:1: main = true"));
        assert!(out.content.contains("README.md:1: # main project"));
        assert!(!out.content.contains(".git/"));
        assert!(!out.content.contains("data.bin"));
        assert!(!out.content.contains("ignored.rs"));
    }

    #[tokio::test]
    async fn pattern_with_path() {
        let (_d, ctx) = setup();
        let out = search(&ctx, json!({"pattern": "fn", "path": "src/nested"}))
            .await
            .unwrap();
        assert_eq!(out.content, "src/nested/lib.rs:1: pub fn helper() {}");
    }

    #[tokio::test]
    async fn truncates_at_max_results() {
        let (_d, ctx) = setup();
        let out = search(&ctx, json!({"pattern": ".", "max_results": 2}))
            .await
            .unwrap();
        assert_eq!(out.preview.len(), 2);
        assert!(out.content.contains("结果超过 2 条，已截断"));
        assert_eq!(out.summary, "2 条结果（已截断）");
    }

    #[tokio::test]
    async fn no_matches() {
        let (_d, ctx) = setup();
        let out = search(&ctx, json!({"pattern": "zzz_nothing"}))
            .await
            .unwrap();
        assert_eq!(out.content, "无匹配结果");
    }

    #[tokio::test]
    async fn argument_errors() {
        let (_d, ctx) = setup();
        assert!(matches!(
            search(&ctx, json!({})).await,
            Err(ToolError::InvalidArgs(_))
        ));
        assert!(
            matches!(search(&ctx, json!({"pattern": "("})).await, Err(ToolError::InvalidArgs(m)) if m.contains("正则"))
        );
        assert!(
            matches!(search(&ctx, json!({"glob": "[", "pattern": "x"})).await, Err(ToolError::InvalidArgs(m)) if m.contains("glob"))
        );
        assert!(
            matches!(search(&ctx, json!({"glob": "*", "path": "nope"})).await, Err(ToolError::Failed(m)) if m.contains("不存在"))
        );
    }
}
