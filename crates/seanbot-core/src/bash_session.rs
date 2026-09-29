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
        for (_, session) in sessions.iter_mut() {
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
    child: Child,
    /// 写端；关掉它（置 None）会让 shell 读到 EOF 自行退出
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    pid: Option<u32>,
    cwd: String,
    created: Instant,
    last_used: Instant,
}

impl Session {
    async fn spawn(name: &str) -> Result<Self, SessionError> {
        let program = if cfg!(windows) {
            return Err(SessionError::Unsupported("Windows 上的常驻会话还没实现"));
        } else if let Some(bash) = find_bash() {
            bash
        } else {
            PathBuf::from("sh")
        };

        let mut command = tokio::process::Command::new(&program);
        command
            .arg("-s")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        // 给测试脚本一个统一的标记：扫描残留进程时按它过滤
        command.env("SEANBOT_BASH_SESSION", name);

        let mut child = command
            .spawn()
            .map_err(|e| SessionError::Spawn(format!("{}: {e}", program.display())))?;
        let pid = child.id();
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| SessionError::Spawn("拿不到 stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| SessionError::Spawn("拿不到 stdout".into()))?;
        // stderr 也读出来，避免管道写满把会话卡死；在 shell 里合并到 stdout 更好
        let stderr = child.stderr.take();
        drop(stderr);

        let mut session = Self {
            name: name.to_string(),
            child,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
            pid,
            cwd: String::new(),
            created: Instant::now(),
            last_used: Instant::now(),
        };
        // 让 shell 自己把 stderr 并到 stdout，保持输出顺序（与一次性 bash 一致）；
        // 再挂一个 EXIT 兜底：shell 无论怎么退出（含 EOF 自杀），都把整个进程组收掉，
        // 包括它启动的后台任务——这样父进程被 SIGKILL 也不会留下孙进程。
        session
            .write("exec 2>&1\ntrap 'kill 0' EXIT\n")
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
        let framed = format!("{command}\nprintf '\\n{MARKER}%s__%s__\\n' \"$?\" \"$PWD\"\n");
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
                Ok(SessionOutput {
                    output: truncate(collected.trim_end_matches('\n'), max_output),
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

    /// 结束会话：先杀进程组（含后台子进程），再确保进程被回收。
    fn shutdown(&mut self) {
        kill_group(self.pid);
        let _ = self.child.start_kill();
        let _ = self.child.try_wait();
        // 关掉写端：即使 kill 没送到，shell 也会读到 EOF 自行退出
        self.stdin = None;
    }
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
