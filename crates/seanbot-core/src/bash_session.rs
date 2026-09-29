//! bash 常驻会话：可以开多个命名会话，程序退出时必须全部清理——**绝不留孤儿进程**。
//!
//! 清理的六层保证：
//! 1. **显式**：CLI 的每条退出路径都会调 `shutdown()`；工具 `bash_session close_all` 也能关
//! 2. **Drop**：`BashSessions` 被丢弃时逐个结束进程组（正常返回、报错返回、panic 展开都会走到）
//! 3. **EOF 兜底**：子 shell 的 stdin 是我们持有的管道；父进程无论怎么消失（包括被 SIGKILL），
//!    管道写端都会关闭，shell 读到 EOF 自行退出（它自己启动的后台作业不吃 EOF，所以只算兜底）
//! 4. **进程组**：每个会话独占一个进程组，关闭时 `killpg(SIGKILL)`，连同它启动的后台子进程一起收掉
//! 5. **退出钩子**：会话会登记到一个进程级名单，`libc::atexit` 里再收一遍——
//!    `std::process::exit()` 跳过 Drop，但会跑退出钩子，"程序自己退出"的路不靠子 shell 自觉
//! 6. **看门狗**：父进程被硬杀（SIGKILL）时退出钩子也跑不到，会话 shell 就自己盯着父进程，
//!    发现它没了立刻 `kill -KILL 0` 端掉整个进程组（连同看门狗自己）
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
        // 会话跑起来了才登记：`std::process::exit()` 之类跳过 Drop 的退出路径也收得掉它
        exit_guard::register(session.pid, &session.token);
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

    /// 结束会话：先按 ppid 树收掉逃逸进程，再杀进程组，最后按标记补一遍。
    fn shutdown(&mut self) {
        // 顺序很重要：逃逸进程此刻还挂在会话 shell 下（setsid 不改 ppid），
        // 一旦端掉进程组，它们会被 reparent 到 init，就再也找不回来了。
        kill_descendants(self.pid);
        kill_group(self.pid);
        // 自己 setsid 脱离进程组的进程不在组里：Linux 上还能按标记扫 /proc 补漏
        kill_escaped(&self.token);
        let _ = self.child.start_kill();
        let _ = self.child.try_wait();
        // 已经收干净了：从退出钩子的名单里划掉，免得多收一遍（也免得 pid 被复用后误伤）
        exit_guard::unregister(&self.token);
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
                // 3) 看门狗：父进程被硬杀时上面两层都赶不上——Rust 端连退出钩子都跑不到，
                //    会话 shell 启动的后台作业又不吃 EOF；所以让一个后台子 shell 盯着
                //    父进程与自己的存活，谁没了就 kill -KILL 0 把整组端掉（含它自己）。
                concat!(
                    "exec 2>&1\n",
                    // 父进程（Seanbot）的 pid：看门狗靠它判断"外面还在不在"
                    "__sb_parent=$PPID\n",
                    // 按 ppid 树收掉子孙进程：macOS 读不到别的进程的环境变量（内核限制），
                    // 这是找到「自己 setsid 逃逸」进程的唯一办法；必须在 kill 0 之前跑，
                    // 那时逃逸进程还挂在会话 shell 下面。
                    "__sb_kill_descendants() {\n",
                    "  ps -o pid=,ppid= -ax 2>/dev/null | awk -v me=$$ '\n",
                    "    { parent[$1] = $2 }\n",
                    "    END {\n",
                    "      mine[me] = 1\n",
                    "      for (n = 0; n < 40; n++) {\n",
                    "        grown = 0\n",
                    "        for (p in parent) if (!(p in mine) && (parent[p] in mine)) { mine[p] = 1; grown = 1 }\n",
                    "        if (!grown) break\n",
                    "      }\n",
                    "      for (p in mine) if (p != me) print p\n",
                    "    }' | while read -r p; do kill -9 \"$p\" 2>/dev/null; done\n",
                    "}\n",
                    // 标记扫描：Linux 上能读到 /proc/<pid>/environ，可以补掉已经不挂在
                    // 会话 shell 下（被 reparent）的漏网进程；macOS 上读不到，基本靠上一步。
                    "__sb_kill_escaped() {\n",
                    "  [ -n \"$SEANBOT_BASH_SESSION_TOKEN\" ] || return 0\n",
                    "  needle=\"SEANBOT_BASH_SESSION_TOKEN=$SEANBOT_BASH_SESSION_TOKEN\"\n",
                    "  if [ -d /proc ]; then\n",
                    "    for environ in /proc/[0-9]*/environ; do\n",
                    "      grep -qzFx \"$needle\" \"$environ\" 2>/dev/null || continue\n",
                    "      pid=\"${environ#/proc/}\"; pid=\"${pid%/environ}\"\n",
                    "      [ \"$pid\" = \"$$\" ] || kill -9 \"$pid\" 2>/dev/null\n",
                    "    done\n",
                    "  fi\n",
                    "}\n",
                    "trap '__sb_kill_descendants; __sb_kill_escaped; kill 0; exit 0' EXIT\n",
                    // 看门狗：`$$` 在子 shell 里仍是会话 shell 的 pid；两者都还在就继续等，
                    // 任一消失（父进程被硬杀 / 会话 shell 被外部杀掉）就端掉整组——SIGKILL 不可忽略
                    "( while kill -0 \"$$\" 2>/dev/null && kill -0 \"$__sb_parent\" 2>/dev/null; do sleep 1; done; kill -KILL 0 ) >/dev/null 2>&1 &\n",
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

/// 收掉挂在会话 shell 下的所有子孙进程（含自己 setsid 的那类）：按 ppid 走一遍进程树。
///
/// 必须赶在 `kill_group` 之前调用——进程组一端掉，逃逸进程就被 reparent 到 init，
/// 再也找不回来。为什么不靠标记扫环境变量：macOS 内核不允许读**别的进程**的环境
/// （`KERN_PROCARGS2` 只对调用者自己返回，`ps eww` 同理），brew 里的进程工具走的也是同一套
/// 系统调用，同样读不到；按 ppid 找则完全可用（`setsid` 不会改 ppid）。
#[cfg(unix)]
fn kill_descendants(pid: Option<u32>) {
    let Some(root) = pid else { return };
    let Ok(output) = std::process::Command::new("ps")
        .args(["-o", "pid=,ppid=", "-ax"])
        .output()
    else {
        return;
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let pairs: Vec<(u32, u32)> = text
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
        })
        .collect();

    // 从会话 shell 出发做一遍可达性搜索（进程数很少，迭代到不动点即可）
    let mut marked = vec![root];
    loop {
        let before = marked.len();
        for (pid, ppid) in &pairs {
            if marked.contains(ppid) && !marked.contains(pid) {
                marked.push(*pid);
            }
        }
        if marked.len() == before {
            break;
        }
    }
    for pid in marked.iter().skip(1) {
        unsafe {
            libc::kill(*pid as libc::pid_t, libc::SIGKILL);
        }
    }
}

/// Windows 靠 `taskkill /T` 杀整棵进程树，不需要额外遍历。
#[cfg(windows)]
fn kill_descendants(_pid: Option<u32>) {}

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
                if let Ok(bytes) = std::fs::read(entry.path().join("environ"))
                    && contains_env(&bytes, &needle)
                {
                    found.push(pid);
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

/// 进程级兜底：`std::process::exit()` 会跳过 Drop，但会跑 `atexit` 钩子。
///
/// 没这层的话，"程序自己退出"就得指望子 shell 读到 stdin EOF 后自觉收尾——实测靠不住
/// （会话 shell 启动的后台作业被 bash 重定向到 /dev/null，根本不看 stdin），所以退出前
/// 自己动手，按 `shutdown()` 同样的顺序把登记过的会话收掉。
/// 万一某个平台在退出时不跑 atexit，也还有 Drop、看门狗、EOF 三层接着。
mod exit_guard {
    use std::sync::{Mutex, Once};

    use super::{kill_descendants, kill_escaped, kill_group};

    /// 活跃会话：进程组 pid + 会话标记（标记用来补掉已经脱离进程组的进程）
    static ACTIVE: Mutex<Vec<(Option<u32>, String)>> = Mutex::new(Vec::new());
    static HOOK: Once = Once::new();

    /// 记下新会话；第一次调用时装好退出钩子。
    pub(super) fn register(pid: Option<u32>, token: &str) {
        HOOK.call_once(|| unsafe {
            libc::atexit(cleanup);
        });
        if let Ok(mut active) = ACTIVE.lock() {
            active.retain(|(_, item)| item != token);
            active.push((pid, token.to_string()));
        }
    }

    /// 会话已经收干净了，从名单里划掉（免得退出时再杀一遍，也免得 pid 被复用后误伤）。
    pub(super) fn unregister(token: &str) {
        if let Ok(mut active) = ACTIVE.lock() {
            active.retain(|(_, item)| item != token);
        }
    }

    /// 这个会话还在退出钩子的名单里吗（只给单测用）。
    #[cfg(test)]
    pub(super) fn is_tracked(token: &str) -> bool {
        ACTIVE
            .lock()
            .map(|active| active.iter().any(|(_, item)| item == token))
            .unwrap_or(false)
    }

    /// 退出钩子的实际动作；单测直接调它，免得真把测试进程退掉。
    pub(super) fn cleanup_now() {
        // 退出期间别的线程可能正持着锁：拿不到就算了，各自的 shutdown/Drop 会兜住，
        // 绝不在钩子里阻塞
        let Ok(active) = ACTIVE.try_lock() else {
            return;
        };
        for (pid, token) in active.iter() {
            // 顺序与 Session::shutdown 一致：先按 ppid 树收（逃逸进程这时还挂在 shell 下），
            // 再端进程组，最后按标记补一遍
            kill_descendants(*pid);
            kill_group(*pid);
            kill_escaped(token);
        }
    }

    /// `atexit` 的入口：必须是 `extern "C"`，里面只做清理、不碰别的状态。
    extern "C" fn cleanup() {
        cleanup_now();
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 会话初始化脚本是拼进 Rust 字符串的 shell 片段：转义写错时这里先炸，
    /// 不用等会话行为测试（`scripts/tests/bash_session_test.sh`）里才发现。
    #[cfg(unix)]
    #[test]
    fn warmup_script_is_valid_bash() {
        let has_bash = std::process::Command::new("bash")
            .arg("--version")
            .output()
            .is_ok();
        if !has_bash {
            return; // 没有 bash 的平台跳过
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("warmup.sh"), ShellKind::Posix.warmup()).unwrap();
        // 用相对路径 + current_dir：免得把 Windows 路径塞给 Git Bash
        let out = std::process::Command::new("bash")
            .arg("-n")
            .arg("warmup.sh")
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "warmup 脚本语法错误：{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// 退出兜底要能收掉「脱离进程组」的漏网进程：macOS 读不到别的进程的环境变量，
    /// 所以必须靠 ppid 树遍历；Linux 再用 /proc 标记扫描补漏。
    #[test]
    fn warmup_reaps_escaped_processes() {
        let script = ShellKind::Posix.warmup();
        assert!(script.contains("__sb_kill_descendants()"), "{script}");
        assert!(script.contains("ps -o pid=,ppid= -ax"), "{script}");
        assert!(script.contains("/proc/[0-9]*/environ"), "{script}");
        assert!(
            script.contains("trap '__sb_kill_descendants; __sb_kill_escaped; kill 0; exit 0' EXIT"),
            "{script}"
        );
    }

    /// 父进程被硬杀时 Rust 端一行代码都跑不到：会话 shell 必须自己盯梢、自己端掉整组。
    #[test]
    fn warmup_has_orphan_watchdog() {
        let script = ShellKind::Posix.warmup();
        assert!(script.contains("__sb_parent=$PPID"), "{script}");
        assert!(script.contains("kill -0 \"$$\""), "{script}");
        assert!(script.contains("kill -0 \"$__sb_parent\""), "{script}");
        assert!(script.contains("kill -KILL 0"), "{script}");
    }

    /// `std::process::exit()` 跳过 Drop，靠退出钩子收尾：登记过的会话要能被收掉，
    /// 收掉之后要划出名单，且钩子在没有会话时执行也必须安全。
    #[test]
    fn exit_guard_tracks_live_sessions() {
        exit_guard::cleanup_now(); // 名单为空时也要能安全执行
        exit_guard::register(None, "guard-test");
        assert!(exit_guard::is_tracked("guard-test"));
        // 没有 pid、标记也扫不到任何进程：钩子跑一遍不应 panic、更不该误杀
        exit_guard::cleanup_now();
        exit_guard::unregister("guard-test");
        assert!(!exit_guard::is_tracked("guard-test"));
        // 重复登记同名会话（关了再开）不会在名单里留两行
        exit_guard::register(None, "guard-test");
        exit_guard::register(None, "guard-test");
        exit_guard::unregister("guard-test");
        assert!(!exit_guard::is_tracked("guard-test"));
    }
}
