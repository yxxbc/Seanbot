use std::{path::PathBuf, process::Stdio, time::Duration};

use async_trait::async_trait;
use seanbot_provider::ToolSpec;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

use crate::{
    denylist::Denylist,
    tool::{Risk, Tool, ToolContext, ToolError, ToolOutput, opt_u64, str_arg},
};

const DEFAULT_TIMEOUT: u64 = 120;
const MAX_TIMEOUT: u64 = 600;
const MAX_OUTPUT: usize = 30_000;
const KEEP: usize = 15_000;
const TITLE_CHARS: usize = 80;

pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "bash".into(),
            description: "在工作目录中执行 shell 命令（bash -c），返回合并后的 stdout 与 stderr 以及退出码。每次调用都是独立进程，不保留 cd 与环境变量；需要时用 && 串联。默认超时 120 秒，可用 timeout 调整（最多 600 秒）。输出超过 30000 字符时只保留首尾。部分危险命令被黑名单禁止。".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "要执行的命令"},
                    "timeout": {"type": "integer", "description": "超时秒数，默认 120，最大 600"}
                },
                "required": ["command"]
            }),
        }
    }

    fn risk(&self) -> Risk {
        Risk::Mutating
    }

    fn title(&self, args: &Value) -> String {
        let cmd = args
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let first = cmd.lines().next().unwrap_or_default();
        let mut title: String = first.chars().take(TITLE_CHARS).collect();
        if first.chars().count() > TITLE_CHARS || cmd.lines().count() > 1 {
            title.push('…');
        }
        title
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let command = str_arg(&args, "command")?;
        if command.trim().is_empty() {
            return Err(ToolError::InvalidArgs("command 不能为空".into()));
        }
        let timeout = opt_u64(&args, "timeout")?
            .unwrap_or(DEFAULT_TIMEOUT)
            .clamp(1, MAX_TIMEOUT);
        Denylist::new(&ctx.config.tools.bash.deny)
            .check(command)
            .map_err(ToolError::Failed)?;

        let mut cmd = tokio::process::Command::new(shell());
        // exec 2>&1：在 shell 内部把 stderr 合并进 stdout，保持输出先后顺序
        cmd.arg("-c")
            .arg(format!("exec 2>&1\n{command}"))
            .current_dir(&ctx.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = cmd
            .spawn()
            .map_err(|e| ToolError::Failed(format!("无法启动 shell：{e}")))?;
        let pid = child.id();
        let mut stdout = child.stdout.take().expect("stdout 已设置为 piped");

        let run = async {
            let mut buf = Vec::new();
            let _ = stdout.read_to_end(&mut buf).await;
            let status = child.wait().await;
            (buf, status)
        };
        let (buf, status) = tokio::select! {
            r = run => r,
            _ = tokio::time::sleep(Duration::from_secs(timeout)) => {
                kill_group(pid);
                return Err(ToolError::Timeout(timeout));
            }
            _ = ctx.cancel.cancelled() => {
                kill_group(pid);
                return Err(ToolError::Cancelled);
            }
        };
        let status = status.map_err(|e| ToolError::Failed(format!("等待进程结束失败：{e}")))?;

        let code = status.code();
        let code_text = code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "无（被信号终止）".into());
        let text = truncate_output(&String::from_utf8_lossy(&buf));
        let body = text.trim_end();
        let content = if body.is_empty() {
            format!("(无输出)\n[退出码 {code_text}]")
        } else {
            format!("{body}\n[退出码 {code_text}]")
        };
        Ok(ToolOutput {
            content,
            summary: format!("退出码 {code_text}"),
            preview: body.lines().map(str::to_string).collect(),
            is_error: code != Some(0),
        })
    }
}

pub(crate) fn truncate_output(s: &str) -> String {
    let total = s.chars().count();
    if total <= MAX_OUTPUT {
        return s.to_string();
    }
    let head: String = s.chars().take(KEEP).collect();
    let tail: String = s.chars().skip(total - KEEP).collect();
    format!("{head}\n…[省略 {} 字符]…\n{tail}", total - 2 * KEEP)
}

/// 优先使用 PATH 中的 bash，没有则用 sh。
fn shell() -> PathBuf {
    std::env::var_os("PATH")
        .and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|d| d.join("bash"))
                .find(|p| p.is_file())
        })
        .unwrap_or_else(|| PathBuf::from("sh"))
}

/// 杀掉整个进程组（包括命令启动的后台子进程）。
fn kill_group(pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        unsafe {
            libc::killpg(pid as libc::pid_t, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    let _ = pid;
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::Instant;

    fn ctx() -> (tempfile::TempDir, ToolContext) {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        (dir, ctx)
    }

    async fn run(ctx: &ToolContext, command: &str) -> Result<ToolOutput, ToolError> {
        BashTool.call(json!({"command": command}), ctx).await
    }

    #[tokio::test]
    async fn merges_stdout_and_stderr_in_order() {
        let (_d, ctx) = ctx();
        let out = run(&ctx, "echo one; echo two >&2; echo three")
            .await
            .unwrap();
        assert_eq!(out.content, "one\ntwo\nthree\n[退出码 0]");
        assert_eq!(out.summary, "退出码 0");
        assert_eq!(out.preview, vec!["one", "two", "three"]);
        assert!(!out.is_error);
    }

    #[tokio::test]
    async fn nonzero_exit_is_error_output() {
        let (_d, ctx) = ctx();
        let out = run(&ctx, "echo bad; exit 3").await.unwrap();
        assert!(out.content.ends_with("[退出码 3]"));
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn runs_in_cwd_without_persistent_state() {
        let (dir, ctx) = ctx();
        let expected = std::fs::canonicalize(dir.path()).unwrap();
        let out = run(&ctx, "pwd -P").await.unwrap();
        assert!(out.content.starts_with(&expected.display().to_string()));
        run(&ctx, "export SEANBOT_T=1; cd /").await.unwrap();
        let out = run(&ctx, "echo \"[$SEANBOT_T]\"; pwd -P").await.unwrap();
        assert!(
            out.content
                .starts_with(&format!("[]\n{}", expected.display()))
        );
    }

    #[tokio::test]
    async fn stdin_is_empty() {
        let (_d, ctx) = ctx();
        let started = Instant::now();
        let out = run(&ctx, "cat; read x; echo done").await.unwrap();
        assert!(out.content.starts_with("done"));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn empty_output() {
        let (_d, ctx) = ctx();
        assert_eq!(
            run(&ctx, "true").await.unwrap().content,
            "(无输出)\n[退出码 0]"
        );
    }

    #[tokio::test]
    async fn timeout_kills_process_group() {
        let (dir, ctx) = ctx();
        let pidfile = dir.path().join("pid");
        let started = Instant::now();
        let command = format!("sleep 30 & echo $! > {}; wait", pidfile.display());
        let err = BashTool
            .call(json!({"command": command, "timeout": 1}), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Timeout(1)));
        assert!(started.elapsed() < Duration::from_secs(5));
        tokio::time::sleep(Duration::from_millis(300)).await;
        let pid: i32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        assert!(!alive, "后台子进程 {pid} 应已被杀死");
    }

    #[tokio::test]
    async fn cancel_stops_command() {
        let (_d, ctx) = ctx();
        let token = ctx.cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            token.cancel();
        });
        let started = Instant::now();
        let err = run(&ctx, "sleep 30").await.unwrap_err();
        assert!(matches!(err, ToolError::Cancelled));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn denylist_enforced_inside_tool() {
        let (_d, ctx) = ctx();
        let err = run(&ctx, "rm -rf somewhere").await.unwrap_err();
        assert!(err.to_string().starts_with("命令被黑名单拒绝："));
    }

    #[test]
    fn truncates_long_output_keeping_head_and_tail() {
        let s = format!("{}{}", "a".repeat(20_000), "b".repeat(20_000));
        let t = truncate_output(&s);
        assert!(t.starts_with(&"a".repeat(15_000)));
        assert!(t.ends_with(&"b".repeat(15_000)));
        assert!(t.contains("…[省略 10000 字符]…"));
        assert_eq!(truncate_output("short"), "short");
    }

    #[test]
    fn title_uses_first_line() {
        assert_eq!(
            BashTool.title(&json!({"command": "cargo build"})),
            "cargo build"
        );
        assert_eq!(BashTool.title(&json!({"command": "a\nb"})), "a…");
    }
}
