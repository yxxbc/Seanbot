//! bash 常驻会话：可以开多个命名会话，程序退出时必须全部清理——**绝不留孤儿进程**。
//!
//! 清理的四层保证：
//! 1. **显式**：CLI 的每条退出路径都会调 `shutdown()`；工具 `bash_session close_all` 也能关
//! 2. **Drop**：`BashSessions` 被丢弃时逐个结束进程组（正常返回、报错返回、panic 展开都会走到）
//! 3. **EOF 兜底**：子 shell 的 stdin 是我们持有的管道；父进程无论怎么消失（包括被 SIGKILL），
//!    管道写端都会关闭，shell 读到 EOF 自行退出
//! 4. **进程组**：每个会话独占一个进程组，关闭时 `killpg(SIGKILL)`，连同它启动的后台子进程一起收掉
//!
//! 注意第 3 层管不到"会话里跑起来的、已经脱离进程组的孙进程"（例如自己 setsid 的守护进程）；
//! 第 4 层能收掉留在同一进程组里的后台任务。测试脚本 scripts/tests/bash_session_test.sh 专门验证这些。

use std::{
    collections::HashMap,
    io,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout},
};
use tokio_util::sync::CancellationToken;

/// 每条命令结束后用于取回退出码与工作目录的哨兵前缀。
const MARKER: &str = "__SEANBOT_DONE__";
/// 超时后给管道留的排空时间。
const DRAIN_GRACE: Duration = Duration::from_millis(200);
/// 会话名的长度上限。
const MAX_NAME_CHARS: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("会话名不合法：{0}（1-32 个字符，只用小写字母、数字、- 和 _，且以字母或数字开头）")]
    BadName(String),
    #[error("会话数已达上限 {0}；先关掉不用的：bash_session close <名字>")]
    TooMany(usize),
    #[error("启动会话失败：{0}")]
    Spawn(String),
    #[error("会话 {0} 出错：{1}")]
    Io(String, String),
    #[error("当前平台暂不支持常驻会话（{0}）；请不带 session 执行，或用一次性的 bash 调用")]
    Unsupported(&'static str),
}

/// 会话信息（给 bash_session list 用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub name: String,
    pub pid: Option<u32>,
    /// 会话最近一次报告的工作目录
    pub cwd: String,
    /// 距离上次使用过了多久
    pub idle_secs: u64,
    /// 会话建立了多久
    pub age_secs: u64,
}

/// 一次会话内命令的执行结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionOutput {
    pub output: String,
    /// 命令退出码（哨兵里带回来的）
    pub exit_code: Option<i32>,
    /// 命令结束时的工作目录
    pub cwd: String,
    /// 命令超时：会话已关闭（进程状态不可信）
    pub timed_out: bool,
    /// 被取消：会话已关闭
    pub cancelled: bool,
}

impl SessionOutput {
    pub fn ok(&self) -> bool {
        !self.timed_out && !self.cancelled && self.exit_code == Some(0)
    }
}

/// 会话集合。克隆共享同一份状态（Agent 持有一份，每次工具调用克隆进 ToolContext）。
#[derive(Debug, Clone)]
pub struct BashSessions {
    inner: Arc<Mutex<HashMap<String, Session>>>,
    max: usize,
}

impl Default for BashSessions {
    fn default() -> Self {
        Self::new(crate::config::DEFAULT_MAX_BASH_SESSIONS)
    }
}

impl BashSessions {
    pub fn new(max: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            max: max.max(1),
        }
    }

    pub fn max(&self) -> usize {
        self.max
    }

    /// 在某个会话里执行命令；会话不存在就按名字创建。
    pub async fn run(
        &self,
        name: &str,
        command: &str,
        timeout: Duration,
        max_output: usize,
        cancel: &CancellationToken,
    ) -> Result<SessionOutput, SessionError> {
        let name = valid_name(name)?;
        let mut session = match self.take(&name)? {
            Some(session) => session,
            None => self.start(&name).await?,
        };
        let result = session.run(command, timeout, max_output, cancel).await;
        match result {
            Ok(output) if !output.timed_out && !output.cancelled => {
                self.put(session);
                Ok(output)
            }
            Ok(output) => {
                // 超时/取消后进程状态不可信：关掉，不留在池子里
                session.shutdown();
                Ok(output)
            }
            Err(e) => {
                session.shutdown();
                Err(e)
            }
        }
    }

    /// 列出所有活跃会话。
    pub fn list(&self) -> Vec<SessionInfo> {
        let sessions = match self.inner.lock() {
            Ok(sessions) => sessions,
            Err(poisoned) => poisoned.into_inner(),
        };
        let mut out: Vec<SessionInfo> = sessions.values().map(Session::info).collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// 关闭一个会话；返回是否真的关掉了。
    pub fn close(&self, name: &str) -> Result<bool, SessionError> {
        let name = valid_name(name)?;
        let mut session = match self.take(&name)? {
            Some(session) => session,
            None => return Ok(false),
        };
        session.shutdown();
        Ok(true)
    }

    /// 关闭全部会话，返回被关掉的名字。
    pub fn close_all(&self) -> Vec<String> {
        let mut sessions = match self.inner.lock() {
            Ok(mut sessions) => std::mem::take(&mut *sessions),
            Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
        };
        let mut names: Vec<String> = sessions.keys().cloned().collect();
        for session in sessions.values_mut() {
            session.shutdown();
        }
        names.sort();
        names
    }

    /// 同步关闭全部会话：退出路径与 Drop 用。
    pub fn shutdown(&self) {
        self.close_all();
    }

    /// 把会话从集合里取出来（执行期间不持锁，避免跨 await 持锁）。
    fn take(&self, name: &str) -> Result<Option<Session>, SessionError> {
        let mut sessions = self
            .inner
            .lock()
            .map_err(|e| SessionError::Io(name.into(), e.to_string()))?;
        Ok(sessions.remove(name))
    }

    fn put(&self, session: Session) {
        if let Ok(mut sessions) = self.inner.lock() {
            sessions.insert(session.name.clone(), session);
        }
    }

    async fn start(&self, name: &str) -> Result<Session, SessionError> {
        {
            let sessions = self
                .inner
                .lock()
                .map_err(|e| SessionError::Io(name.into(), e.to_string()))?;
            if sessions.len() >= self.max {
                return Err(SessionError::TooMany(self.max));
            }
        }
        Session::spawn(name).await
    }
}

impl Drop for BashSessions {
    fn drop(&mut self) {
        // 只有最后一个持有者负责收尾，避免克隆体各自关闭别人的会话
        if Arc::strong_count(&self.inner) == 1 {
            self.shutdown();
        }
    }
}

/// 单个常驻会话。
struct Session {
    name: String,
    /// 本会话唯一的进程标记（写进子进程环境，用于清理脱离进程组的漏网进程）
    token: String,
    kind: ShellKind,
    child: Child,
    /// 写端；关掉它（置 None）会让 shell 读到 EOF 自行退出
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    /// 后台读出来的 stderr（Unix 上 shell 已把 stderr 并进 stdout，这里通常为空）
    stderr_tail: Arc<Mutex<String>>,
    pid: Option<u32>,
    cwd: String,
    created: Instant,
    last_used: Instant,
}

impl Session {
    async fn spawn(name: &str) -> Result<Self, SessionError> {
        let kind = ShellKind::detect()?;
        // 每个会话一个唯一标记：清理时按它把"脱离进程组"的漏网进程也扫出来
        let token = format!("{name}-{}-{}", std::process::id(), next_session_id());

        let mut command = kind.command();
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        command
            .env("SEANBOT_BASH_SESSION", name)
            .env(TOKEN_ENV, &token);

        let mut child = command
            .spawn()
            .map_err(|e| SessionError::Spawn(e.to_string()))?;
        let pid = child.id();
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| SessionError::Spawn("拿不到 stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| SessionError::Spawn("拿不到 stdout".into()))?;
        let stderr_tail = Arc::new(Mutex::new(String::new()));
        if let Some(stderr) = child.stderr.take() {
            spawn_stderr_reader(stderr, Arc::clone(&stderr_tail));
        }

        let mut session = Self {
            name: name.to_string(),
            token,
            kind,
            child,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
            stderr_tail,
            pid,
            cwd: String::new(),
            created: Instant::now(),
            last_used: Instant::now(),
        };
        session
            .write(&kind.warmup())
            .await
            .map_err(|e| SessionError::Spawn(e.to_string()))?;
        Ok(session)
    }

    async fn write(&mut self, text: &str) -> io::Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "会话已关闭，无法写入"))?;
        stdin.write_all(text.as_bytes()).await?;
        stdin.flush().await
    }

    async fn run(
        &mut self,
        command: &str,
        timeout: Duration,
        max_output: usize,
        cancel: &CancellationToken,
    ) -> Result<SessionOutput, SessionError> {
        self.last_used = Instant::now();
        let framed = self.kind.frame(command);
        self.write(&framed)
            .await
            .map_err(|e| SessionError::Io(self.name.clone(), e.to_string()))?;

        // 读取 future 借用了 self，结果先移出这个作用域再动 self 的其它字段
        let outcome = {
            let read = self.read_until_marker();
            tokio::pin!(read);
            tokio::select! {
                biased;
                _ = cancel.cancelled() => Outcome::Cancelled,
                _ = tokio::time::sleep(timeout) => Outcome::TimedOut,
                result = &mut read => match result {
                    Ok((output, exit_code, cwd)) => Outcome::Done(output, exit_code, cwd),
                    Err(e) => Outcome::Failed(e.to_string()),
                },
            }
        };

        match outcome {
            Outcome::Failed(reason) => Err(SessionError::Io(self.name.clone(), reason)),
            Outcome::Done(collected, exit_code, cwd) => {
                self.cwd = cwd.clone();
                let mut output = collected;
                self.append_stderr(&mut output);
                Ok(SessionOutput {
                    output: truncate(output.trim_end_matches('\n'), max_output),
                    exit_code: Some(exit_code),
                    cwd,
                    timed_out: false,
                    cancelled: false,
                })
            }
            Outcome::TimedOut => {
                let drained = self.drain().await;
                Ok(SessionOutput {
                    output: truncate(drained.trim_end_matches('\n'), max_output),
                    exit_code: None,
                    cwd: self.cwd.clone(),
                    timed_out: true,
                    cancelled: false,
                })
            }
            Outcome::Cancelled => Ok(SessionOutput {
                output: String::new(),
                exit_code: None,
                cwd: self.cwd.clone(),
                timed_out: false,
                cancelled: true,
            }),
        }
    }

    /// 读到哨兵行，返回（命令输出、退出码、工作目录）。
    async fn read_until_marker(&mut self) -> io::Result<(String, i32, String)> {
        let mut collected = String::new();
        loop {
            let mut line = String::new();
            let read = self.stdout.read_line(&mut line).await?;
            if read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "会话进程已退出（可能被外部杀掉）",
                ));
            }
            if let Some((code, cwd)) = parse_marker(line.trim_end_matches(['\n', '\r'])) {
                return Ok((collected, code, cwd));
            }
            collected.push_str(&line);
        }
    }

    /// 超时后杀掉进程组，再把管道里剩下的输出读干净。
    async fn drain(&mut self) -> String {
        kill_group(self.pid);
        let mut out = String::new();
        let reader = &mut self.stdout;
        let _ = tokio::time::timeout(DRAIN_GRACE, async {
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => out.push_str(&line),
                }
            }
        })
        .await;
        out
    }

    fn info(&self) -> SessionInfo {
        SessionInfo {
            name: self.name.clone(),
            pid: self.pid,
            cwd: self.cwd.clone(),
            idle_secs: self.last_used.elapsed().as_secs(),
            age_secs: self.created.elapsed().as_secs(),
        }
    }

    /// 取走后台读到的 stderr，拼到输出后面。
    fn append_stderr(&self, output: &mut String) {
        let text = match self.stderr_tail.lock() {
            Ok(mut tail) => std::mem::take(&mut *tail),
            Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
        };
        let text = text.trim_end();
        if text.is_empty() {
            return;
        }
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str(text);
        output.push('\n');
    }

    /// 结束会话：先杀进程组（含后台子进程），再收掉脱离进程组的漏网进程。
    fn shutdown(&mut self) {
        kill_group(self.pid);
        // 自己 setsid 脱离进程组的进程不在组里，按标记再扫一遍
        kill_escaped(&self.token);
        let _ = self.child.start_kill();
        let _ = self.child.try_wait();
        // 关掉写端：即使 kill 没送到，shell 也会读到 EOF 自行退出
        self.stdin = None;
    }
}

/// 写进每个会话 shell（及其所有子孙）环境的标记变量名。
const TOKEN_ENV: &str = "SEANBOT_BASH_SESSION_TOKEN";
/// 后台暂存的 stderr 上限，防止刷屏把内存吃光。
const MAX_STDERR_TAIL: usize = 64 * 1024;

/// 会话用的 shell 类型：分帧与收尾方式不同。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellKind {
    /// bash / sh：`-s` 从 stdin 读命令
    Posix,
    /// Windows PowerShell：`-Command -` 从 stdin 读命令
    PowerShell,
}

impl ShellKind {
    fn detect() -> Result<Self, SessionError> {
        if cfg!(windows) && find_bash().is_none() {
            if std::env::var_os("PATH").is_none() {
                return Err(SessionError::Spawn(
                    "PATH 取不到，找不到可用的 shell".into(),
                ));
            }
            return Ok(Self::PowerShell);
        }
        Ok(Self::Posix)
    }

    fn command(self) -> tokio::process::Command {
        match self {
            Self::Posix => {
                let program = find_bash().unwrap_or_else(|| PathBuf::from("sh"));
                let mut command = tokio::process::Command::new(program);
                command.arg("-s");
                command
            }
            Self::PowerShell => {
                let mut command = tokio::process::Command::new("powershell");
                command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", "-"]);
                command
            }
        }
    }

    /// 会话刚建立时先发的初始化脚本。
    fn warmup(self) -> String {
        match self {
            Self::Posix => {
                // 1) stderr 并进 stdout，保持输出顺序（与一次性 bash 一致）
                // 2) EXIT 兜底：shell 无论怎么退出（含 stdin EOF 自杀）都
                //    - 按标记扫掉"脱离进程组"的漏网进程（自己 setsid 的那种）
                //    - kill 0 收掉整个进程组
                //    这样即使父进程被 SIGKILL、Rust 端清理代码没机会跑，
                //    会话里的进程（含后台任务与逃逸进程）也会被 shell 自己收走。
                concat!(
                    "exec 2>&1\n",
                    "__sb_kill_escaped() {\n",
                    "  [ -n \"$SEANBOT_BASH_SESSION_TOKEN\" ] || return 0\n",
                    "  ps eww -ax 2>/dev/null | grep -F \"SEANBOT_BASH_SESSION_TOKEN=$SEANBOT_BASH_SESSION_TOKEN\" \\\n",
                    "    | grep -v grep | awk -v me=$$ '$1 != me { print $1 }' \\\n",
                    "    | while read -r p; do kill -9 \"$p\" 2>/dev/null; done\n",
                    "}\n",
                    "trap '__sb_kill_escaped; kill 0' EXIT\n",
                )
                .to_string()
            }
            Self::PowerShell => "$ProgressPreference = 'SilentlyContinue'\n".to_string(),
        }
    }

    /// 把一条命令包成"命令 + 哨兵"，哨兵里带回退出码与当前目录。
    fn frame(self, command: &str) -> String {
        match self {
            Self::Posix => {
                format!("{command}\nprintf '\\n{MARKER}%s__%s__\\n' \"$?\" \"$PWD\"\n")
            }
            Self::PowerShell => format!(
                "$LASTEXITCODE = $null\n{command}\n\
                 $__sb = if ($LASTEXITCODE -ne $null) {{ $LASTEXITCODE }} elseif ($?) {{ 0 }} else {{ 1 }}\n\
                 Write-Output (\"`n{MARKER}\" + $__sb + \"__\" + (Get-Location).Path + \"__\")\n"
            ),
        }
    }
}

/// 会话序号：让每个会话的标记互不相同（旧会话的漏网进程不会被新会话误杀）。
fn next_session_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// 后台读 stderr：一是避免管道写满把会话卡死，二是把错误输出并回结果里。
fn spawn_stderr_reader(mut stderr: tokio::process::ChildStderr, tail: Arc<Mutex<String>>) {
    tokio::spawn(async move {
        let mut reader = BufReader::new(&mut stderr);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if let Ok(mut guard) = tail.lock() {
                        guard.push_str(&line);
                        if guard.len() > MAX_STDERR_TAIL {
                            let cut = guard.len() - MAX_STDERR_TAIL;
                            let safe = (cut..guard.len())
                                .find(|index| guard.is_char_boundary(*index))
                                .unwrap_or(guard.len());
                            guard.drain(..safe);
                        }
                    }
                }
            }
        }
    });
}

/// 收掉"脱离进程组"的漏网进程：按本会话唯一的标记扫环境变量。
///
/// 进程组能收掉绝大多数子孙；主动 setsid 的进程不在组里，但环境变量会被继承下来，
/// 所以再扫一遍把它们 SIGKILL 掉。注意：对方若显式清空了环境（env -i 之类），
/// 就只剩进程组与 EOF 两层兜底了。
#[cfg(unix)]
fn kill_escaped(token: &str) {
    for pid in marked_processes(token) {
        if pid == std::process::id() {
            continue;
        }
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGKILL);
        }
    }
}

/// Windows 上没有可移植的"按环境变量找进程"办法：靠 taskkill /T 杀进程树。
#[cfg(windows)]
fn kill_escaped(_token: &str) {}

/// 找出带着本会话标记的进程。
#[cfg(unix)]
fn marked_processes(token: &str) -> Vec<u32> {
    let needle = format!("{TOKEN_ENV}={token}");
    let mut found = Vec::new();

    #[cfg(target_os = "linux")]
    {
        if let Ok(entries) = std::fs::read_dir("/proc") {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(pid) = name.to_str().and_then(|n| n.parse::<u32>().ok()) else {
                    continue;
                };
                if let Ok(bytes) = std::fs::read(entry.path().join("environ")) {
                    if contains_env(&bytes, &needle) {
                        found.push(pid);
                    }
                }
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        // macOS / 其它 Unix：ps eww 会把环境变量打出来
        if let Ok(out) = std::process::Command::new("ps")
            .args(["eww", "-ax"])
            .output()
        {
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                if !line.contains(&needle) {
                    continue;
                }
                if let Some(pid) = line
                    .split_whitespace()
                    .next()
                    .and_then(|first| first.parse::<u32>().ok())
                {
                    found.push(pid);
                }
            }
        }
    }

    found.sort_unstable();
    found.dedup();
    found
}

/// 环境块是 NUL 分隔的 KEY=VALUE，按整项匹配避免子串误判。
#[cfg(target_os = "linux")]
fn contains_env(environ: &[u8], needle: &str) -> bool {
    environ
        .split(|byte| *byte == 0)
        .any(|entry| entry == needle.as_bytes())
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("name", &self.name)
            .field("pid", &self.pid)
            .field("cwd", &self.cwd)
            .finish_non_exhaustive()
    }
}

enum Outcome {
    Done(String, i32, String),
    TimedOut,
    Cancelled,
    Failed(String),
}

/// 解析哨兵行：`__SEANBOT_DONE__<退出码>__<工作目录>__`。
fn parse_marker(line: &str) -> Option<(i32, String)> {
    let rest = line.trim_start().strip_prefix(MARKER)?;
    let (code, rest) = rest.split_once("__")?;
    let cwd = rest.strip_suffix("__")?;
    Some((code.trim().parse().ok()?, cwd.to_string()))
}

/// 会话名规则：1-32 个字符，小写字母/数字/-/_，以字母或数字开头。
pub fn valid_name(raw: &str) -> Result<String, SessionError> {
    let name = raw.trim();
    let first_ok = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let ok = first_ok
        && name.chars().count() <= MAX_NAME_CHARS
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !ok {
        return Err(SessionError::BadName(raw.to_string()));
    }
    Ok(name.to_string())
}

fn find_bash() -> Option<PathBuf> {
    let name = if cfg!(windows) { "bash.exe" } else { "bash" };
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(name))
            .find(|path| path.is_file())
    })
}

/// 结束整个进程组（Unix）或进程树（Windows）。
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

/// 与一次性 bash 一致：超长输出保留首尾。
fn truncate(text: &str, max_chars: usize) -> String {
    let total = text.chars().count();
    if total <= max_chars {
        return text.to_string();
    }
    let keep = max_chars / 2;
    let head: String = text.chars().take(keep).collect();
    let tail: String = text.chars().skip(total - keep).collect();
    format!("{head}\n…[省略 {} 字符]…\n{tail}", total - 2 * keep)
}
