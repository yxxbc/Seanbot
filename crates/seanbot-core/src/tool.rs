//! 工具接口与共享的工具上下文。

use std::{
    collections::HashMap,
    io,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use seanbot_provider::ToolSpec;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{
    config::Config,
    runtime::{RuntimeState, SharedRuntime, shared_runtime},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolSource {
    Builtin,
    AgentCreated,
    Plugin { id: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Risk {
    ReadOnly,
    Mutating,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput {
    /// 交给模型
    pub content: String,
    /// UI 一行摘要
    pub summary: String,
    /// UI 折叠预览行
    pub preview: Vec<String>,
    /// 工具运行完成但结果表示失败（如命令退出码非 0）
    pub is_error: bool,
}

impl ToolOutput {
    pub fn new(content: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            summary: summary.into(),
            preview: Vec::new(),
            is_error: false,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("{0}")]
    InvalidArgs(String),
    #[error("{0}")]
    Failed(String),
    #[error("用户已取消")]
    Cancelled,
    #[error("命令执行超时（{0} 秒），已终止整个进程组")]
    Timeout(u64),
}

/// 读取文件时记录的指纹：字节数与内容哈希。
///
/// 只比对内容，不比对修改时间：编辑器保存、`cargo fmt`、上一次 `edit` 都会改动 mtime，
/// 只要内容逐字节相同，就没有重新读取的必要。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileSnapshot {
    pub len: u64,
    pub hash: u64,
}

impl FileSnapshot {
    pub fn of(bytes: &[u8]) -> Self {
        Self {
            len: bytes.len() as u64,
            hash: content_hash(bytes),
        }
    }
}

/// FNV-1a 64 位：这里只需要判断"内容有没有变"，不值得引入依赖。
fn content_hash(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// 记录本会话读过的文件内容指纹，edit 据此判断是否需要重新读取。
#[derive(Debug, Clone, Default)]
pub struct ReadTracker {
    inner: Arc<Mutex<HashMap<PathBuf, FileSnapshot>>>,
}

impl ReadTracker {
    fn key(path: &Path) -> PathBuf {
        std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    }

    pub fn record(&self, path: &Path, snapshot: FileSnapshot) {
        self.inner.lock().unwrap().insert(Self::key(path), snapshot);
    }

    pub fn get(&self, path: &Path) -> Option<FileSnapshot> {
        self.inner.lock().unwrap().get(&Self::key(path)).copied()
    }

    pub fn clear(&self) {
        self.inner.lock().unwrap().clear();
    }
}

#[derive(Debug, Clone)]
pub struct ToolContext {
    pub cwd: PathBuf,
    pub cancel: CancellationToken,
    pub reads: ReadTracker,
    pub config: Arc<Config>,
    pub runtime: SharedRuntime,
}

impl ToolContext {
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            cancel: CancellationToken::new(),
            reads: ReadTracker::default(),
            config: Arc::new(Config::default()),
            runtime: shared_runtime(RuntimeState::default()),
        }
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    fn source(&self) -> ToolSource {
        ToolSource::Builtin
    }
    fn risk(&self) -> Risk;
    /// UI 显示用的参数摘要，例如 bash 的命令、read 的路径。
    fn title(&self, args: &Value) -> String;
    /// 供后续确认界面使用，本版 CLI 不调用。
    async fn preview(&self, _args: &Value, _ctx: &ToolContext) -> Option<String> {
        None
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError>;
}

/// 相对路径以工作目录为基准。
pub fn resolve_path(cwd: &Path, raw: &str) -> PathBuf {
    let p = Path::new(raw);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    }
}

/// 判断 `path` 是否位于 `cwd` 之内：按真实路径判断（解析符号链接与 `..`），
/// 目标不存在时对不存在的部分按字面拼接。
pub fn is_within(cwd: &Path, path: &Path) -> bool {
    let base = std::fs::canonicalize(cwd).unwrap_or_else(|_| lexical_normalize(cwd));
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    resolve_real(&joined).starts_with(&base)
}

/// 从左到右逐段解析：`..` 作用在已解析符号链接的真实位置上，已存在的部分随时规范化为真实路径。
/// 反过来先做词法压平会把 `link/../x`（link 指向工作目录之外）误判为工作目录内。
fn resolve_real(path: &Path) -> PathBuf {
    let mut real = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                real.pop();
            }
            other => {
                real.push(other.as_os_str());
                if let Ok(resolved) = std::fs::canonicalize(&real) {
                    real = resolved;
                }
            }
        }
    }
    real
}

/// 只做词法处理：去掉 `.`，把 `..` 与前一段抵消。
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

pub(crate) fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::InvalidArgs(format!("缺少字符串参数 {key}")))
}

pub(crate) fn opt_str<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_str()
            .map(Some)
            .ok_or_else(|| ToolError::InvalidArgs(format!("参数 {key} 必须是字符串"))),
    }
}

pub(crate) fn opt_u64(args: &Value, key: &str) -> Result<Option<u64>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .map(Some)
            .ok_or_else(|| ToolError::InvalidArgs(format!("参数 {key} 必须是非负整数"))),
    }
}

pub(crate) fn opt_bool(args: &Value, key: &str) -> Result<Option<bool>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_bool()
            .map(Some)
            .ok_or_else(|| ToolError::InvalidArgs(format!("参数 {key} 必须是布尔值"))),
    }
}

pub(crate) fn io_error(raw: &str, e: io::Error) -> ToolError {
    if e.kind() == io::ErrorKind::NotFound {
        ToolError::Failed(format!("文件不存在：{raw}"))
    } else {
        ToolError::Failed(format!("访问 {raw} 失败：{e}"))
    }
}

/// 前 8KB 含 NUL 字节即视为二进制。
pub(crate) fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8192).any(|&b| b == 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[cfg(unix)]
    #[test]
    fn resolve_relative_and_absolute() {
        let cwd = Path::new("/work");
        assert_eq!(
            resolve_path(cwd, "src/a.rs"),
            PathBuf::from("/work/src/a.rs")
        );
        assert_eq!(resolve_path(cwd, "/etc/hosts"), PathBuf::from("/etc/hosts"));
    }

    #[test]
    fn tracker_normalizes_paths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        let file = dir.path().join("src/a.txt");
        std::fs::write(&file, "x").unwrap();
        let t = ReadTracker::default();
        let snapshot = FileSnapshot::of(b"x");
        t.record(&dir.path().join("./src/../src/a.txt"), snapshot);
        assert_eq!(t.get(&file), Some(snapshot));
        t.clear();
        assert_eq!(t.get(&file), None);
    }

    #[test]
    fn snapshot_follows_content_not_time() {
        let snapshot = FileSnapshot::of(b"hello");
        assert_eq!(snapshot.len, 5);
        assert_eq!(snapshot, FileSnapshot::of(b"hello"));
        assert_ne!(snapshot, FileSnapshot::of(b"hellp"));
        assert_ne!(snapshot, FileSnapshot::of(b"hello "));
        assert_ne!(snapshot, FileSnapshot::of("你好".as_bytes()));
    }

    #[test]
    fn arg_helpers() {
        let args = json!({"s": "x", "n": 3, "b": true, "bad": -1, "nul": null});
        assert_eq!(str_arg(&args, "s").unwrap(), "x");
        assert!(
            matches!(str_arg(&args, "missing"), Err(ToolError::InvalidArgs(m)) if m.contains("missing"))
        );
        assert_eq!(opt_u64(&args, "n").unwrap(), Some(3));
        assert_eq!(opt_u64(&args, "nul").unwrap(), None);
        assert!(opt_u64(&args, "bad").is_err());
        assert_eq!(opt_bool(&args, "b").unwrap(), Some(true));
        assert_eq!(opt_str(&args, "missing").unwrap(), None);
        assert!(opt_str(&args, "n").is_err());
    }

    #[test]
    fn binary_detection() {
        assert!(is_binary(b"ab\0cd"));
        assert!(!is_binary("你好".as_bytes()));
    }

    #[test]
    fn is_within_checks_real_paths() {
        let dir = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        let cwd = dir.path();
        assert!(is_within(cwd, Path::new("a.txt")));
        assert!(is_within(cwd, Path::new("new/dir/x.txt")));
        assert!(is_within(cwd, &dir.path().join("a.txt")));
        assert!(!is_within(cwd, Path::new("../x.txt")));
        assert!(!is_within(cwd, Path::new("sub/../../x.txt")));
        assert!(!is_within(cwd, &other.path().join("y.txt")));
    }

    #[cfg(unix)]
    #[test]
    fn is_within_follows_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(other.path(), dir.path().join("link")).unwrap();
        assert!(!is_within(dir.path(), Path::new("link/f.txt")));
    }

    #[cfg(unix)]
    #[test]
    fn is_within_resolves_parent_after_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(other.path(), dir.path().join("link")).unwrap();
        // `..` 作用于符号链接解析后的真实位置：link/.. 是 other 的父目录，不是 dir
        assert!(!is_within(dir.path(), Path::new("link/../x.txt")));
        // 符号链接指向工作目录内部时，仍判定为内部
        std::fs::create_dir(dir.path().join("real")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("in")).unwrap();
        assert!(is_within(dir.path(), Path::new("in/../x.txt")));
    }
}
