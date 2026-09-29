//! 工具接口与共享的工具上下文。

use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::SystemTime,
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

/// 记录本会话读过的文件及读取时的修改时间，edit 据此判断是否需要重新读取。
#[derive(Debug, Clone, Default)]
pub struct ReadTracker {
    inner: Arc<Mutex<HashMap<PathBuf, SystemTime>>>,
}

impl ReadTracker {
    fn key(path: &Path) -> PathBuf {
        std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    }

    pub fn record(&self, path: &Path, mtime: SystemTime) {
        self.inner.lock().unwrap().insert(Self::key(path), mtime);
    }

    pub fn get(&self, path: &Path) -> Option<SystemTime> {
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
        let now = SystemTime::now();
        t.record(&dir.path().join("./src/../src/a.txt"), now);
        assert_eq!(t.get(&file), Some(now));
        t.clear();
        assert_eq!(t.get(&file), None);
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
}
