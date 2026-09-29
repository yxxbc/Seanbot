use std::{path::Path, process::Stdio, time::Duration};

use async_trait::async_trait;
use seanbot_provider::ToolSpec;
use serde_json::{Value, json};

use crate::tool::{Risk, Tool, ToolContext, ToolError, ToolOutput};

const GIT_TIMEOUT: Duration = Duration::from_secs(3);

pub struct PerceiveTool;

#[async_trait]
impl Tool for PerceiveTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "perceive".into(),
            description: "获取当前运行状态：时间、厂商与模型、权限模式、会话、工作目录、git 状态、本会话用量。这些信息会变化，需要时调用。".into(),
            parameters: json!({"type": "object", "properties": {}}),
        }
    }

    fn risk(&self) -> Risk {
        Risk::ReadOnly
    }

    fn title(&self, _args: &Value) -> String {
        String::new()
    }

    async fn call(&self, _args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let state = ctx.runtime.read().unwrap().clone();
        let session = match (&state.session_id, &state.session_path) {
            (Some(id), Some(path)) => format!("{id}（{}）", path.display()),
            (Some(id), None) => id.clone(),
            _ => "未保存".to_string(),
        };
        let cache = state
            .usage
            .cache_hit_tokens
            .map(|hit| format!("（缓存命中 {hit}）"))
            .unwrap_or_default();
        let lines = vec![
            format!(
                "- 当前时间：{}",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S %:z")
            ),
            format!("- 厂商 / 模型：{} / {}", state.provider, state.model),
            format!("- 权限模式：{}", state.permission_mode.label()),
            format!("- 会话：{session}"),
            format!("- 工作目录：{}", ctx.cwd.display()),
            format!("- git：{}", git_status(&ctx.cwd).await),
            format!(
                "- 本会话用量：输入 {}{cache} · 输出 {}",
                state.usage.input_tokens, state.usage.output_tokens
            ),
        ];
        Ok(ToolOutput {
            content: lines.join("\n"),
            summary: "当前状态".into(),
            preview: lines,
            is_error: false,
        })
    }
}

enum GitError {
    Missing,
    Failed(String),
}

/// 运行一条 git 命令（英文输出、3 秒超时），返回 stdout。
async fn run_git(cwd: &Path, args: &[&str]) -> Result<String, GitError> {
    let child = tokio::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output();
    let output = match tokio::time::timeout(GIT_TIMEOUT, child).await {
        Err(_) => return Err(GitError::Failed("查询超时".into())),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => return Err(GitError::Missing),
        Ok(Err(e)) => return Err(GitError::Failed(e.to_string())),
        Ok(Ok(o)) => o,
    };
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(GitError::Failed(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ))
    }
}

async fn git_status(cwd: &Path) -> String {
    let branch = match run_git(cwd, &["branch", "--show-current"]).await {
        Ok(b) => b.trim().to_string(),
        Err(GitError::Missing) => return "git 不可用（未安装）".into(),
        Err(GitError::Failed(msg)) if msg.contains("not a git repository") => {
            return "不是 git 仓库".into();
        }
        Err(GitError::Failed(msg)) => return format!("查询失败：{msg}"),
    };
    let branch = if branch.is_empty() {
        "（分离的 HEAD）".to_string()
    } else {
        branch
    };
    match run_git(cwd, &["status", "--porcelain"]).await {
        Ok(s) => match s.lines().filter(|l| !l.trim().is_empty()).count() {
            0 => format!("分支 {branch}，工作区干净"),
            n => format!("分支 {branch}，{n} 个文件有改动"),
        },
        Err(GitError::Failed(msg)) => format!("分支 {branch}，改动查询失败：{msg}"),
        Err(GitError::Missing) => "git 不可用（未安装）".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::PermissionMode;
    use seanbot_provider::Usage;

    fn has_git() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_ok()
    }

    #[tokio::test]
    async fn reports_runtime_state() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        {
            let mut s = ctx.runtime.write().unwrap();
            s.provider = "deepseek".into();
            s.model = "deepseek-flash".into();
            s.permission_mode = PermissionMode::Yolo;
            s.usage = Usage {
                input_tokens: 1200,
                output_tokens: 34,
                cache_hit_tokens: Some(1000),
                cache_miss_tokens: Some(200),
            };
        }
        let out = PerceiveTool.call(json!({}), &ctx).await.unwrap();
        let text = &out.content;
        assert!(text.starts_with("- 当前时间："), "{text}");
        assert!(
            text.contains("- 厂商 / 模型：deepseek / deepseek-flash"),
            "{text}"
        );
        assert!(text.contains("- 权限模式：YOLO"), "{text}");
        assert!(text.contains("- 会话：未保存"), "{text}");
        assert!(
            text.contains(&format!("- 工作目录：{}", dir.path().display())),
            "{text}"
        );
        assert!(
            text.contains("- 本会话用量：输入 1200（缓存命中 1000） · 输出 34"),
            "{text}"
        );
        assert_eq!(out.preview.len(), 7);
        assert_eq!(out.summary, "当前状态");

        {
            let mut s = ctx.runtime.write().unwrap();
            s.session_id = Some("20260929-120000-abcdef".into());
            s.session_path = Some("/tmp/s.jsonl".into());
        }
        let out = PerceiveTool.call(json!({}), &ctx).await.unwrap();
        assert!(
            out.content
                .contains("- 会话：20260929-120000-abcdef（/tmp/s.jsonl）"),
            "{}",
            out.content
        );
    }

    #[tokio::test]
    async fn git_outside_repository() {
        if !has_git() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(git_status(dir.path()).await, "不是 git 仓库");
    }

    #[tokio::test]
    async fn git_branch_and_changes() {
        if !has_git() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .unwrap()
        };
        git(&["init", "-q", "-b", "main"]);
        assert_eq!(git_status(dir.path()).await, "分支 main，工作区干净");
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        std::fs::write(dir.path().join("b.txt"), "y").unwrap();
        assert_eq!(git_status(dir.path()).await, "分支 main，2 个文件有改动");
    }
}
