//! 会话持久化：`<数据目录>/sessions/<目录标识>/<会话ID>.jsonl`，每行一条记录，只追加。

use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU32, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use seanbot_provider::{Message, Role};
use serde::{Deserialize, Serialize};

use crate::config::{self, ConfigError};

const INTERRUPTED_TOOL: &str = "会话中断，工具未执行";
const FIRST_PROMPT_CHARS: usize = 80;

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("会话文件读写失败：{0}")]
    Io(#[from] io::Error),
    #[error("会话记录序列化失败：{0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Config(#[from] ConfigError),
    #[error("找不到会话 {0}")]
    NotFound(String),
    #[error("会话文件缺少 meta 记录：{0}")]
    MissingMeta(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub cwd: PathBuf,
    pub created_at: String,
    pub provider: String,
    pub model: String,
    /// 完整系统提示词；恢复时原样使用，保证请求前缀逐字节一致
    pub system: String,
}

impl SessionMeta {
    pub fn new(
        cwd: &Path,
        provider: impl Into<String>,
        model: impl Into<String>,
        system: impl Into<String>,
    ) -> Self {
        Self {
            id: new_session_id(),
            cwd: cwd.to_path_buf(),
            created_at: now_rfc3339(),
            provider: provider.into(),
            model: model.into(),
            system: system.into(),
        }
    }
}

fn now_rfc3339() -> String {
    chrono::Local::now().to_rfc3339()
}

pub fn new_session_id() -> String {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let salt = nanos
        ^ std::process::id().rotate_left(13)
        ^ COUNTER
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_mul(0x9E37_79B9);
    format!(
        "{}-{:06x}",
        chrono::Local::now().format("%Y%m%d-%H%M%S"),
        salt & 0xFF_FFFF
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolUi {
    pub call_id: String,
    pub name: String,
    pub title: String,
    pub ok: bool,
    pub summary: String,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Record {
    Meta(SessionMeta),
    Message(Message),
    ToolUi(ToolUi),
    Clear {
        at: String,
    },
    Model {
        model: String,
    },
    /// 撤回最后一条消息（本轮首步取消/失败时未被回应的用户消息）
    Retract,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionSummary {
    pub id: String,
    pub path: PathBuf,
    pub first_prompt: String,
    pub updated_at: SystemTime,
    pub messages: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoadedSession {
    pub meta: SessionMeta,
    pub history: Vec<Message>,
    pub tool_ui: HashMap<String, ToolUi>,
    pub model: String,
    pub warnings: Vec<String>,
    /// 为缺失结果的工具调用补上的消息（已包含在 `history` 末尾）
    pub repaired: Vec<Message>,
}

/// 工作目录 → 目录标识：保留 Unicode 字母数字，其余字符替换为 `-`。
pub fn dir_key(cwd: &Path) -> String {
    let path = fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    path.to_string_lossy()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect()
}

#[derive(Debug, Clone)]
pub struct SessionStore {
    root: PathBuf,
}

impl SessionStore {
    pub fn open_default() -> Result<Self, SessionError> {
        Ok(Self::at(config::data_dir()?.join("sessions")))
    }

    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn dir_for(&self, cwd: &Path) -> PathBuf {
        self.root.join(dir_key(cwd))
    }

    pub fn create(&self, mut meta: SessionMeta) -> Result<SessionWriter, SessionError> {
        let dir = self.dir_for(&meta.cwd);
        create_private_dir(&dir)?;
        for attempt in 0..8 {
            if attempt > 0 {
                meta.id = new_session_id();
            }
            let path = dir.join(format!("{}.jsonl", meta.id));
            match open_new_private(&path) {
                Ok(file) => {
                    let mut writer = SessionWriter { path, file };
                    writer.append(&Record::Meta(meta))?;
                    return Ok(writer);
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Err(io::Error::new(io::ErrorKind::AlreadyExists, "无法生成唯一的会话 ID").into())
    }

    pub fn list(&self, cwd: &Path) -> Result<Vec<SessionSummary>, SessionError> {
        let entries = match fs::read_dir(self.dir_for(cwd)) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        for entry in entries {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            // 单个文件损坏不影响列出其他会话
            if let Ok(summary) = summarize(&path) {
                out.push(summary);
            }
        }
        out.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        Ok(out)
    }

    pub fn find(&self, cwd: &Path, id: &str) -> Result<PathBuf, SessionError> {
        let not_found = || SessionError::NotFound(id.to_string());
        if id.is_empty() || id.contains(['/', '\\']) || id.contains("..") {
            return Err(not_found());
        }
        let file = format!("{id}.jsonl");
        let here = self.dir_for(cwd).join(&file);
        if here.is_file() {
            return Ok(here);
        }
        let entries = match fs::read_dir(&self.root) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(not_found()),
            Err(e) => return Err(e.into()),
        };
        for entry in entries {
            let candidate = entry?.path().join(&file);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
        Err(not_found())
    }

    pub fn latest(&self, cwd: &Path) -> Result<Option<SessionSummary>, SessionError> {
        Ok(self.list(cwd)?.into_iter().next())
    }
}

#[derive(Debug)]
pub struct SessionWriter {
    path: PathBuf,
    file: File,
}

impl SessionWriter {
    pub fn open_append(path: &Path) -> Result<Self, SessionError> {
        let mut file = OpenOptions::new().read(true).append(true).open(path)?;
        // 上次写到一半崩溃时，最后一行没有换行：先补一个，避免新记录与残缺行粘连
        let len = file.metadata()?.len();
        if len > 0 {
            let mut last = [0u8; 1];
            file.seek(SeekFrom::Start(len - 1))?;
            file.read_exact(&mut last)?;
            if last[0] != b'\n' {
                file.write_all(b"\n")?;
            }
        }
        Ok(Self {
            path: path.to_path_buf(),
            file,
        })
    }

    /// 写入一条记录并立即 flush：崩溃时最多丢失最后一条。
    pub fn append(&mut self, record: &Record) -> Result<(), SessionError> {
        let mut line = serde_json::to_string(record)?;
        line.push('\n');
        self.file.write_all(line.as_bytes())?;
        self.file.flush()?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn id(&self) -> String {
        self.path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

pub fn load_session(path: &Path) -> Result<LoadedSession, SessionError> {
    let reader = BufReader::new(File::open(path)?);
    let mut meta = None;
    let mut history = Vec::new();
    let mut tool_ui = HashMap::new();
    let mut model = None;
    let mut warnings = Vec::new();
    for (i, line) in reader.split(b'\n').enumerate() {
        let line = line?;
        let text = String::from_utf8_lossy(&line);
        if text.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Record>(&text) {
            Ok(Record::Meta(m)) => {
                if meta.is_none() {
                    meta = Some(m);
                }
            }
            Ok(Record::Message(m)) => history.push(m),
            Ok(Record::ToolUi(t)) => {
                tool_ui.insert(t.call_id.clone(), t);
            }
            Ok(Record::Clear { .. }) => history.clear(),
            Ok(Record::Model { model: m }) => model = Some(m),
            Ok(Record::Retract) => {
                history.pop();
            }
            Err(_) => warnings.push(format!("第 {} 行无法解析，已跳过", i + 1)),
        }
    }
    let meta = meta.ok_or_else(|| SessionError::MissingMeta(path.display().to_string()))?;
    let repaired = missing_tool_results(&history);
    history.extend(repaired.iter().cloned());
    Ok(LoadedSession {
        model: model.unwrap_or_else(|| meta.model.clone()),
        meta,
        history,
        tool_ui,
        warnings,
        repaired,
    })
}

pub fn resume_session(path: &Path) -> Result<(LoadedSession, SessionWriter), SessionError> {
    let loaded = load_session(path)?;
    let mut writer = SessionWriter::open_append(path)?;
    for m in &loaded.repaired {
        writer.append(&Record::Message(m.clone()))?;
    }
    Ok((loaded, writer))
}

/// 最后一条助手消息的工具调用若缺少结果（进程在执行工具时退出），为其补上"会话中断"。
fn missing_tool_results(history: &[Message]) -> Vec<Message> {
    let Some(idx) = history.iter().rposition(|m| m.role == Role::Assistant) else {
        return Vec::new();
    };
    let after = &history[idx + 1..];
    if after.iter().any(|m| m.role != Role::Tool) {
        return Vec::new();
    }
    let answered: HashSet<&str> = after
        .iter()
        .filter_map(|m| m.tool_call_id.as_deref())
        .collect();
    history[idx]
        .tool_calls
        .iter()
        .filter(|c| !answered.contains(c.id.as_str()))
        .map(|c| Message::tool(&c.id, INTERRUPTED_TOOL))
        .collect()
}

fn summarize(path: &Path) -> Result<SessionSummary, SessionError> {
    let loaded = load_session(path)?;
    let first_prompt = loaded
        .history
        .iter()
        .find(|m| m.role == Role::User)
        .map(|m| clip_line(&m.content, FIRST_PROMPT_CHARS))
        .unwrap_or_else(|| "（空会话）".to_string());
    Ok(SessionSummary {
        id: loaded.meta.id,
        path: path.to_path_buf(),
        first_prompt,
        updated_at: fs::metadata(path)?.modified()?,
        messages: loaded.history.len(),
    })
}

/// 把多行文本压成一行并截到 `max` 个字符（超出时以 … 结尾）。
fn clip_line(s: &str, max: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let mut out: String = flat.chars().take(max).collect();
    out.push('…');
    out
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn open_new_private(path: &Path) -> io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.append(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use seanbot_provider::ToolCall;

    fn meta(cwd: &Path) -> SessionMeta {
        SessionMeta::new(cwd, "deepseek", "deepseek-flash", "# 身份\n系统提示词")
    }

    fn store() -> (tempfile::TempDir, SessionStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::at(dir.path().join("sessions"));
        (dir, store)
    }

    fn msg(m: Message) -> Record {
        Record::Message(m)
    }

    #[test]
    fn id_format_and_dir_key() {
        let id = new_session_id();
        let parts: Vec<_> = id.split('-').collect();
        assert_eq!(parts.len(), 3, "{id}");
        assert_eq!(parts[0].len(), 8);
        assert_eq!(parts[1].len(), 6);
        assert_eq!(parts[2].len(), 6);
        assert_eq!(
            dir_key(Path::new("/nonexistent-seanbot/项目 A/x.y")),
            "-nonexistent-seanbot-项目-A-x-y"
        );
    }

    #[test]
    fn record_json_shape() {
        let v = serde_json::to_value(msg(Message::user("hi"))).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"type":"message","role":"user","content":"hi"})
        );
        assert_eq!(
            serde_json::to_value(Record::Retract).unwrap(),
            serde_json::json!({"type":"retract"})
        );
    }

    #[test]
    fn roundtrip_preserves_messages() {
        let (_d, store) = store();
        let cwd = Path::new("/nonexistent-seanbot/proj");
        let m = meta(cwd);
        let mut w = store.create(m.clone()).unwrap();
        let history = vec![
            Message::user("你好\n第二行 \"引号\" 🎉"),
            Message::assistant(
                "",
                Some("想一想".into()),
                vec![ToolCall {
                    id: "c1".into(),
                    name: "read".into(),
                    arguments: r#"{"path":"a"}"#.into(),
                }],
            ),
            Message::tool("c1", "     1\thello"),
            Message::assistant("完成", None, vec![]),
        ];
        for h in &history {
            w.append(&msg(h.clone())).unwrap();
        }
        let ui = ToolUi {
            call_id: "c1".into(),
            name: "read".into(),
            title: "a".into(),
            ok: true,
            summary: "读取 1 行".into(),
            elapsed_ms: 3,
        };
        w.append(&Record::ToolUi(ui.clone())).unwrap();

        let loaded = load_session(w.path()).unwrap();
        assert_eq!(loaded.meta.system, m.system);
        assert_eq!(loaded.meta.cwd, m.cwd);
        assert_eq!(loaded.history, history);
        assert_eq!(loaded.tool_ui.get("c1"), Some(&ui));
        assert_eq!(loaded.model, "deepseek-flash");
        assert!(loaded.warnings.is_empty());
        assert!(loaded.repaired.is_empty());
    }

    #[test]
    fn clear_retract_and_model_records() {
        let (_d, store) = store();
        let mut w = store.create(meta(Path::new("/x"))).unwrap();
        w.append(&msg(Message::user("A"))).unwrap();
        w.append(&msg(Message::assistant("a", None, vec![])))
            .unwrap();
        w.append(&Record::Clear { at: "t".into() }).unwrap();
        w.append(&msg(Message::user("B"))).unwrap();
        w.append(&msg(Message::assistant("b", None, vec![])))
            .unwrap();
        w.append(&msg(Message::user("C"))).unwrap();
        w.append(&Record::Retract).unwrap();
        w.append(&Record::Model {
            model: "deepseek-v4-pro".into(),
        })
        .unwrap();
        let loaded = load_session(w.path()).unwrap();
        assert_eq!(
            loaded.history,
            vec![Message::user("B"), Message::assistant("b", None, vec![])]
        );
        assert_eq!(loaded.model, "deepseek-v4-pro");
    }

    #[test]
    fn truncated_last_line_is_skipped_and_repaired_on_append() {
        let (_d, store) = store();
        let mut w = store.create(meta(Path::new("/x"))).unwrap();
        w.append(&msg(Message::user("A"))).unwrap();
        let path = w.path().to_path_buf();
        drop(w);
        // 模拟写到一半崩溃：最后一行不完整且没有换行
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(br#"{"type":"message","role"#).unwrap();
        drop(f);

        let loaded = load_session(&path).unwrap();
        assert_eq!(loaded.history, vec![Message::user("A")]);
        assert_eq!(loaded.warnings.len(), 1);
        assert!(
            loaded.warnings[0].contains("第 3 行"),
            "{:?}",
            loaded.warnings
        );

        let (_, mut w) = resume_session(&path).unwrap();
        w.append(&msg(Message::assistant("a", None, vec![])))
            .unwrap();
        let loaded = load_session(&path).unwrap();
        assert_eq!(
            loaded.history,
            vec![Message::user("A"), Message::assistant("a", None, vec![])]
        );
        assert_eq!(loaded.warnings.len(), 1, "新记录不应与残缺行粘连");
    }

    #[test]
    fn repairs_missing_tool_results() {
        let (_d, store) = store();
        let mut w = store.create(meta(Path::new("/x"))).unwrap();
        let calls = vec![
            ToolCall {
                id: "c1".into(),
                name: "bash".into(),
                arguments: "{}".into(),
            },
            ToolCall {
                id: "c2".into(),
                name: "bash".into(),
                arguments: "{}".into(),
            },
        ];
        w.append(&msg(Message::user("跑两个命令"))).unwrap();
        w.append(&msg(Message::assistant("", None, calls))).unwrap();
        w.append(&msg(Message::tool("c1", "ok"))).unwrap();
        let path = w.path().to_path_buf();
        drop(w);

        let (loaded, _w) = resume_session(&path).unwrap();
        assert_eq!(loaded.repaired, vec![Message::tool("c2", INTERRUPTED_TOOL)]);
        assert_eq!(
            loaded.history.last(),
            Some(&Message::tool("c2", INTERRUPTED_TOOL))
        );
        // 补齐的结果已写入文件：再次加载不再需要修复
        let again = load_session(&path).unwrap();
        assert!(again.repaired.is_empty());
        assert_eq!(again.history.len(), 4);
    }

    #[test]
    fn missing_meta_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.jsonl");
        fs::write(
            &path,
            "{\"type\":\"message\",\"role\":\"user\",\"content\":\"hi\"}\n",
        )
        .unwrap();
        assert!(matches!(
            load_session(&path),
            Err(SessionError::MissingMeta(_))
        ));
    }

    #[test]
    fn create_never_overwrites() {
        let (_d, store) = store();
        let m = meta(Path::new("/x"));
        let a = store.create(m.clone()).unwrap();
        let b = store.create(m).unwrap();
        assert_ne!(a.path(), b.path());
        assert_ne!(a.id(), b.id());
        assert_eq!(load_session(b.path()).unwrap().meta.id, b.id());
    }

    #[test]
    fn list_sorted_and_summarized() {
        let (_d, store) = store();
        let cwd = Path::new("/nonexistent-seanbot/list");
        assert!(store.list(cwd).unwrap().is_empty());

        let mut old = store.create(meta(cwd)).unwrap();
        old.append(&msg(Message::user("旧的问题"))).unwrap();
        let mut new = store.create(meta(cwd)).unwrap();
        let long = "长".repeat(100) + "\n第二行";
        new.append(&msg(Message::user(long))).unwrap();
        new.append(&msg(Message::assistant("答", None, vec![])))
            .unwrap();
        File::options()
            .write(true)
            .open(old.path())
            .unwrap()
            .set_modified(SystemTime::now() - std::time::Duration::from_secs(60))
            .unwrap();

        let list = store.list(cwd).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, new.id());
        assert_eq!(list[0].messages, 2);
        assert_eq!(list[0].first_prompt.chars().count(), FIRST_PROMPT_CHARS + 1);
        assert!(list[0].first_prompt.ends_with('…'));
        assert!(!list[0].first_prompt.contains('\n'));
        assert_eq!(list[1].first_prompt, "旧的问题");
        assert_eq!(store.latest(cwd).unwrap().unwrap().id, new.id());
    }

    #[test]
    fn find_across_directories_and_rejects_paths() {
        let (_d, store) = store();
        let a = Path::new("/nonexistent-seanbot/a");
        let b = Path::new("/nonexistent-seanbot/b");
        let w = store.create(meta(a)).unwrap();
        assert_eq!(store.find(a, &w.id()).unwrap(), w.path());
        assert_eq!(store.find(b, &w.id()).unwrap(), w.path());
        assert!(matches!(
            store.find(a, "20260101-000000-000000"),
            Err(SessionError::NotFound(_))
        ));
        assert!(matches!(
            store.find(a, "../x"),
            Err(SessionError::NotFound(_))
        ));
        assert!(matches!(
            store.find(a, "a/b"),
            Err(SessionError::NotFound(_))
        ));
    }

    #[test]
    fn create_fails_when_root_is_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sessions");
        fs::write(&root, "not a dir").unwrap();
        let store = SessionStore::at(root);
        assert!(matches!(
            store.create(meta(Path::new("/x"))),
            Err(SessionError::Io(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let (_d, store) = store();
        let w = store.create(meta(Path::new("/x"))).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(w.path()), 0o600);
        assert_eq!(mode(w.path().parent().unwrap()), 0o700);
    }
}
