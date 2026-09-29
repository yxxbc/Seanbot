use std::{
    collections::VecDeque,
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

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
/// 输出缓冲每侧最多保留的字节数（字符上限的 4 倍，足以容纳 UTF-8）。
const CAPTURE_BYTES: usize = KEEP * 4;
/// 进程退出后等待输出管道关闭的宽限期。
const DRAIN_GRACE: Duration = Duration::from_millis(200);

/// 执行命令所用的解释器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Interpreter {
    Bash(PathBuf),
    Sh,
    PowerShell,
}

impl Interpreter {
    /// PATH 中有 bash（Windows 上为 Git Bash）时用 bash；否则 Unix 用 sh、Windows 用 PowerShell。
    pub(crate) fn detect() -> Self {
        if let Some(path) = find_bash() {
            return Self::Bash(path);
        }
        if cfg!(windows) {
            Self::PowerShell
        } else {
            Self::Sh
        }
    }

    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::Bash(_) => "bash",
            Self::Sh => "sh",
            Self::PowerShell => "PowerShell",
        }
    }

    fn command(&self, script: &str) -> tokio::process::Command {
        match self {
            Self::Bash(path) => {
                let mut c = tokio::process::Command::new(path);
                // exec 2>&1：在 shell 内部把 stderr 合并进 stdout，保持输出先后顺序
                c.arg("-c").arg(format!("exec 2>&1\n{script}"));
                c
            }
            Self::Sh => {
                let mut c = tokio::process::Command::new("sh");
                c.arg("-c").arg(format!("exec 2>&1\n{script}"));
                c
            }
            Self::PowerShell => {
                let mut c = tokio::process::Command::new("powershell");
                c.args(["-NoProfile", "-NonInteractive", "-Command"])
                    .arg(format!("& {{ {script} }} 2>&1"));
                c
            }
        }
    }
}

fn find_bash() -> Option<PathBuf> {
    let name = if cfg!(windows) { "bash.exe" } else { "bash" };
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join(name))
            .find(|p| p.is_file() && !is_wsl_launcher(p))
    })
}

/// Windows 自带的 `System32\bash.exe` 启动的是 WSL，而不是 Git Bash。
fn is_wsl_launcher(path: &std::path::Path) -> bool {
    cfg!(windows)
        && path
            .to_string_lossy()
            .to_ascii_lowercase()
            .contains("\\windows\\system32\\")
}

/// 当前平台 bash 工具实际使用的解释器名称。
pub(crate) fn interpreter_label() -> &'static str {
    Interpreter::detect().label()
}

#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "bash".into(),
            description: "在工作目录中执行 shell 命令（有 bash 时使用 bash，Windows 上无 Git Bash 时使用 PowerShell），返回合并后的 stdout 与 stderr 以及退出码。每次调用都是独立进程，不保留 cd 与环境变量；需要时用 && 串联。默认超时 120 秒，可用 timeout 调整（最多 600 秒）。输出超过 30000 字符时只保留首尾。部分危险命令被黑名单禁止。".into(),
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

        execute(&Interpreter::detect(), command, timeout, ctx).await
    }
}

/// 用指定解释器执行命令：边读边截断输出，超时或取消时结束整个进程组（Windows 为进程树）。
pub(crate) async fn execute(
    interpreter: &Interpreter,
    command: &str,
    timeout: u64,
    ctx: &ToolContext,
) -> Result<ToolOutput, ToolError> {
    let mut cmd = interpreter.command(command);
    cmd.current_dir(&ctx.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
    let mut child = cmd
        .spawn()
        .map_err(|e| ToolError::Failed(format!("无法启动 {}：{e}", interpreter.label())))?;
    let pid = child.id();
    let mut stdout = child.stdout.take().expect("stdout 已设置为 piped");

    // 边读边截断：内存占用有上限，超时或取消时也能拿到已有输出
    let capture = Arc::new(Mutex::new(Capture::default()));
    let mut reader = {
        let capture = capture.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 8192];
            loop {
                match stdout.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => capture.lock().unwrap().push(&buf[..n]),
                }
            }
        })
    };

    let ended = tokio::select! {
        status = child.wait() => Ended::Exited(status),
        _ = tokio::time::sleep(Duration::from_secs(timeout)) => Ended::TimedOut,
        _ = ctx.cancel.cancelled() => Ended::Cancelled,
    };
    if !matches!(ended, Ended::Exited(_)) {
        kill_group(pid);
    }
    // 后台子进程可能一直占着管道：只给一小段宽限期收尾，不等 EOF
    if tokio::time::timeout(DRAIN_GRACE, &mut reader)
        .await
        .is_err()
    {
        reader.abort();
    }
    let text = capture.lock().unwrap().render();
    let body = text.trim_end();
    let preview: Vec<String> = body.lines().map(str::to_string).collect();

    let status = match ended {
        Ended::Cancelled => return Err(ToolError::Cancelled),
        Ended::TimedOut => {
            let notice = format!("[命令执行超时（{timeout} 秒），已终止整个进程组]");
            let content = if body.is_empty() {
                format!("(无输出)\n{notice}")
            } else {
                format!("{body}\n{notice}")
            };
            return Ok(ToolOutput {
                content,
                summary: format!("超时（{timeout} 秒）"),
                preview,
                is_error: true,
            });
        }
        Ended::Exited(status) => {
            status.map_err(|e| ToolError::Failed(format!("等待进程结束失败：{e}")))?
        }
    };

    let code = status.code();
    let code_text = code
        .map(|c| c.to_string())
        .unwrap_or_else(|| "无（被信号终止）".into());
    let content = if body.is_empty() {
        format!("(无输出)\n[退出码 {code_text}]")
    } else {
        format!("{body}\n[退出码 {code_text}]")
    };
    Ok(ToolOutput {
        content,
        summary: format!("退出码 {code_text}"),
        preview,
        is_error: code != Some(0),
    })
}

enum Ended {
    Exited(std::io::Result<std::process::ExitStatus>),
    TimedOut,
    Cancelled,
}

/// 边读边截断的输出缓冲：只保留开头与结尾各 [`CAPTURE_BYTES`] 字节。
#[derive(Debug, Default)]
pub(crate) struct Capture {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    dropped: usize,
}

impl Capture {
    pub(crate) fn push(&mut self, data: &[u8]) {
        let take = CAPTURE_BYTES
            .saturating_sub(self.head.len())
            .min(data.len());
        self.head.extend_from_slice(&data[..take]);
        self.tail.extend(&data[take..]);
        if self.tail.len() > CAPTURE_BYTES {
            let excess = self.tail.len() - CAPTURE_BYTES;
            self.tail.drain(..excess);
            self.dropped += excess;
        }
    }

    pub(crate) fn render(&self) -> String {
        let tail: Vec<u8> = self.tail.iter().copied().collect();
        if self.dropped == 0 {
            let mut all = self.head.clone();
            all.extend_from_slice(&tail);
            return truncate_output(&String::from_utf8_lossy(&all));
        }
        let head: String = String::from_utf8_lossy(&self.head)
            .chars()
            .take(KEEP)
            .collect();
        let tail = String::from_utf8_lossy(&tail);
        let skip = tail.chars().count().saturating_sub(KEEP);
        let tail: String = tail.chars().skip(skip).collect();
        format!("{head}\n…[省略 {} 字节以上]…\n{tail}", self.dropped)
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

/// 结束整个进程组（Unix）或进程树（Windows），包括命令启动的后台子进程。
fn kill_group(pid: Option<u32>) {
    let Some(pid) = pid else { return };
    #[cfg(unix)]
    unsafe {
        libc::killpg(pid as libc::pid_t, libc::SIGKILL);
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .output();
    }
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
        let out = BashTool
            .call(json!({"command": command, "timeout": 1}), &ctx)
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(
            out.content.contains("命令执行超时（1 秒）"),
            "{}",
            out.content
        );
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
    async fn timeout_keeps_partial_output() {
        let (_d, ctx) = ctx();
        let out = BashTool
            .call(
                json!({"command": "echo partial; sleep 10", "timeout": 1}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.content.starts_with("partial\n"), "{}", out.content);
        assert!(out.content.contains("超时"));
    }

    #[tokio::test]
    async fn backgrounded_command_returns_promptly() {
        let (_d, ctx) = ctx();
        let started = Instant::now();
        let out = BashTool
            .call(
                json!({"command": "sleep 5 & echo started", "timeout": 3}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(out.content, "started\n[退出码 0]");
    }

    #[test]
    fn capture_is_bounded() {
        let mut cap = Capture::default();
        let chunk = vec![b'y'; 64 * 1024];
        for _ in 0..160 {
            cap.push(&chunk); // 共 10 MB
        }
        assert!(cap.head.len() <= CAPTURE_BYTES);
        assert!(cap.tail.len() <= CAPTURE_BYTES);
        let text = cap.render();
        assert!(text.chars().count() < MAX_OUTPUT + 100);
        assert!(text.contains("…[省略"));
        let mut small = Capture::default();
        small.push(b"hello\n");
        assert_eq!(small.render(), "hello\n");
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

    #[tokio::test]
    async fn sh_interpreter_runs_command() {
        let (_d, ctx) = ctx();
        let out = execute(&Interpreter::Sh, "echo from-sh", 10, &ctx)
            .await
            .unwrap();
        assert_eq!(out.content, "from-sh\n[退出码 0]");
    }

    #[test]
    fn detects_bash_on_unix() {
        assert!(matches!(
            Interpreter::detect(),
            Interpreter::Bash(_) | Interpreter::Sh
        ));
        assert_ne!(Interpreter::PowerShell.label(), Interpreter::Sh.label());
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::time::Instant;

    fn ctx() -> (tempfile::TempDir, ToolContext) {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        (dir, ctx)
    }

    #[tokio::test]
    async fn powershell_runs_command() {
        let (_d, ctx) = ctx();
        let out = execute(&Interpreter::PowerShell, "Write-Output hi", 30, &ctx)
            .await
            .unwrap();
        assert!(out.content.starts_with("hi"), "{}", out.content);
        assert!(!out.is_error);
    }

    #[tokio::test]
    async fn powershell_timeout_kills_tree() {
        let (_d, ctx) = ctx();
        let started = Instant::now();
        let out = execute(&Interpreter::PowerShell, "Start-Sleep -Seconds 30", 2, &ctx)
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("超时"), "{}", out.content);
        assert!(started.elapsed() < Duration::from_secs(15));
    }
}
