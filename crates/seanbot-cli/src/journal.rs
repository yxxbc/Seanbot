//! 会话落盘：把内核事件流录进 `<数据目录>/sessions/<目录标识>/<会话ID>.jsonl`。
//!
//! 内核只负责发事件（`AgentEvent::MessageAppended` 等），是否记录、记到哪个文件由这里决定。

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

use seanbot_core::{
    AgentEvent, SharedRuntime,
    session::{
        Record, SessionError, SessionMeta, SessionStore, SessionSummary, SessionWriter, ToolUi,
    },
};

/// 事件 → 会话记录。只保留能重建历史与还原界面的事件；流式增量（TextDelta 等）不入库。
#[derive(Debug, Default)]
struct EventTap {
    /// call_id → (工具名, 标题)；`ToolFinished` 里没有这两项，靠 `ToolStarted` 补齐
    labels: HashMap<String, (String, String)>,
}

impl EventTap {
    fn record(&mut self, event: &AgentEvent) -> Option<Record> {
        match event {
            AgentEvent::MessageAppended(message) => Some(Record::Message(message.clone())),
            AgentEvent::MessageRetracted => Some(Record::Retract),
            AgentEvent::ToolStarted {
                call_id,
                name,
                title,
            } => {
                self.labels
                    .insert(call_id.clone(), (name.clone(), title.clone()));
                None
            }
            AgentEvent::ToolFinished {
                call_id,
                ok,
                summary,
                elapsed,
                ..
            } => {
                let (name, title) = self.labels.remove(call_id).unwrap_or_default();
                Some(Record::ToolUi(ToolUi {
                    call_id: call_id.clone(),
                    name,
                    title,
                    ok: *ok,
                    summary: summary.clone(),
                    elapsed_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
                }))
            }
            AgentEvent::ThinkingStarted
            | AgentEvent::ReasoningDelta(_)
            | AgentEvent::TextDelta(_)
            | AgentEvent::TurnFinished { .. }
            | AgentEvent::Cancelled
            | AgentEvent::Error(_) => None,
        }
    }
}

/// 当前会话的写入端。
///
/// 懒创建：直到第一条真正要落盘的记录出现才建文件，"打开就退出"不会留下空会话。
/// 写失败不打断对话，只在首次失败时提示一次。
pub struct Journal {
    store: SessionStore,
    runtime: SharedRuntime,
    cwd: PathBuf,
    /// 当前会话的元信息模板；`id` 在创建文件时才被真正占用
    meta: SessionMeta,
    writer: Option<SessionWriter>,
    tap: EventTap,
    recording: bool,
    warned: bool,
}

impl Journal {
    pub fn new(
        store: SessionStore,
        runtime: SharedRuntime,
        cwd: PathBuf,
        provider: impl Into<String>,
        model: impl Into<String>,
        system: impl Into<String>,
        recording: bool,
    ) -> Self {
        Self {
            meta: SessionMeta::new(&cwd, provider, model, system),
            store,
            runtime,
            cwd,
            writer: None,
            tap: EventTap::default(),
            recording,
            warned: false,
        }
    }

    pub fn is_recording(&self) -> bool {
        self.recording
    }

    pub fn store(&self) -> &SessionStore {
        &self.store
    }

    /// 已落盘的会话 ID；还没写过任何记录时为 `None`。
    pub fn id(&self) -> Option<String> {
        self.writer.as_ref().map(SessionWriter::id)
    }

    #[cfg(test)]
    pub fn path(&self) -> Option<&Path> {
        self.writer.as_ref().map(SessionWriter::path)
    }

    /// 记录一个内核事件。
    pub fn record(&mut self, event: &AgentEvent) {
        if let Some(record) = self.tap.record(event) {
            self.append(&record);
        }
    }

    /// 追加一条记录。
    pub fn append(&mut self, record: &Record) {
        if !self.recording {
            return;
        }
        match self.write(record) {
            Ok(()) => self.warned = false,
            Err(e) => self.warn_once(&e),
        }
    }

    /// 只在已经落盘时追加：避免 `/clear`、`/model` 变成本次对话的唯一内容。
    pub fn append_if_started(&mut self, record: &Record) {
        if self.writer.is_some() {
            self.append(record);
        }
    }

    /// 开始新会话：丢弃当前写入端，下一条记录会新建文件。
    pub fn start_new(&mut self, model: impl Into<String>, system: impl Into<String>) {
        self.meta = SessionMeta::new(&self.cwd, self.meta.provider.clone(), model, system);
        self.writer = None;
        self.tap = EventTap::default();
        self.warned = false;
        self.publish(None);
    }

    /// 接上已存在的会话文件。
    pub fn bind(&mut self, meta: SessionMeta, writer: SessionWriter) {
        self.meta = meta;
        self.writer = Some(writer);
        self.tap = EventTap::default();
        self.warned = false;
        self.publish(self.writer.as_ref());
    }

    fn open(&mut self) -> Result<(), SessionError> {
        if self.writer.is_some() {
            return Ok(());
        }
        let meta = self.meta.clone();
        let writer = self.store.create(meta)?;
        self.writer = Some(writer);
        self.publish(self.writer.as_ref());
        Ok(())
    }

    fn write(&mut self, record: &Record) -> Result<(), SessionError> {
        self.open()?;
        let writer = self.writer.as_mut().expect("上一步已确保存在");
        writer.append(record)
    }

    /// 提前建好会话文件。
    ///
    /// 事件是由后台任务消费的，如果等到第一条 `MessageAppended` 才建文件，
    /// 模型在同一轮里调用 `perceive` 有可能还看不到会话 ID。
    pub fn ensure_open(&mut self) {
        if !self.recording {
            return;
        }
        if let Err(e) = self.open() {
            self.warn_once(&e);
        }
    }

    fn publish(&self, writer: Option<&SessionWriter>) {
        let mut state = self.runtime.write().unwrap();
        match writer {
            Some(w) => {
                state.session_id = Some(w.id());
                state.session_path = Some(w.path().to_path_buf());
            }
            None => {
                state.session_id = None;
                state.session_path = None;
            }
        }
    }

    fn warn_once(&mut self, e: &SessionError) {
        if !self.warned {
            self.warned = true;
            eprintln!("会话未能写入（同类错误不再提示）：{e}");
        }
    }
}

/// 列出当前目录的会话并让用户选一个；没有会话时返回 `None`。
///
/// 直接读 stdin，用于启动时 `-r` 不带 ID 的场景；交互模式里请改用会话自己的行编辑器。
pub fn pick_session(store: &SessionStore, cwd: &Path) -> anyhow::Result<Option<PathBuf>> {
    let sessions = store.list(cwd)?;
    if sessions.is_empty() {
        println!("当前目录还没有会话记录");
        return Ok(None);
    }
    print_sessions(&sessions);
    let idx = crate::setup::prompt_choice("输入序号（回车选最近一次）：", sessions.len(), Some(0))?;
    Ok(Some(sessions[idx].path.clone()))
}

/// 打印会话列表（启动时选择与 `/resume` 共用）。
pub fn print_sessions(sessions: &[SessionSummary]) {
    println!("选择要恢复的会话：");
    for (i, s) in sessions.iter().enumerate() {
        println!(
            "  {}. {}  {}  {} 条  {}",
            i + 1,
            s.id,
            ago(s.updated_at),
            s.messages,
            s.first_prompt
        );
    }
}

/// 相对时间：一天内用「x 分钟前」，更早用「月-日 时:分」。
pub fn ago(t: SystemTime) -> String {
    let secs = SystemTime::now()
        .duration_since(t)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    match secs {
        0..60 => "刚刚".to_string(),
        60..3600 => format!("{} 分钟前", secs / 60),
        3600..86400 => format!("{} 小时前", secs / 3600),
        _ => chrono::DateTime::<chrono::Local>::from(t)
            .format("%m-%d %H:%M")
            .to_string(),
    }
}

/// 两个路径是否指向同一处；路径不存在时退回字面比较。
pub fn same_dir(a: &Path, b: &Path) -> bool {
    let real = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    real(a) == real(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use seanbot_core::{RuntimeState, session, shared_runtime};
    use seanbot_provider::Message;
    use std::time::Duration;

    fn started(call_id: &str, name: &str, title: &str) -> AgentEvent {
        AgentEvent::ToolStarted {
            call_id: call_id.into(),
            name: name.into(),
            title: title.into(),
        }
    }

    fn finished(call_id: &str, ok: bool, summary: &str, ms: u64) -> AgentEvent {
        AgentEvent::ToolFinished {
            call_id: call_id.into(),
            ok,
            summary: summary.into(),
            preview: Vec::new(),
            elapsed: Duration::from_millis(ms),
        }
    }

    fn journal(dir: &Path, recording: bool) -> (Journal, SharedRuntime, PathBuf) {
        let cwd = dir.join("proj");
        fs::create_dir_all(&cwd).unwrap();
        let runtime = shared_runtime(RuntimeState::default());
        let store = SessionStore::at(dir.join("sessions"));
        let j = Journal::new(
            store,
            runtime.clone(),
            cwd.clone(),
            "deepseek",
            "m",
            "系统",
            recording,
        );
        (j, runtime, cwd)
    }

    #[test]
    fn tap_keeps_history_and_tool_ui() {
        let mut tap = EventTap::default();
        let m = Message::user("你好");
        assert_eq!(
            tap.record(&AgentEvent::MessageAppended(m.clone())),
            Some(Record::Message(m))
        );
        assert_eq!(tap.record(&AgentEvent::TextDelta("你".into())), None);
        assert_eq!(tap.record(&started("c1", "read", "a.rs")), None);
        assert_eq!(
            tap.record(&finished("c1", true, "读取 3 行", 12)),
            Some(Record::ToolUi(ToolUi {
                call_id: "c1".into(),
                name: "read".into(),
                title: "a.rs".into(),
                ok: true,
                summary: "读取 3 行".into(),
                elapsed_ms: 12,
            }))
        );
        assert_eq!(
            tap.record(&AgentEvent::MessageRetracted),
            Some(Record::Retract)
        );
        // ToolFinished 没有对应的 ToolStarted 时不应 panic
        assert!(tap.record(&finished("c9", false, "失败", 36_000)).is_some());
    }

    #[test]
    fn nothing_is_written_until_a_message_arrives() {
        let dir = tempfile::tempdir().unwrap();
        let (mut j, runtime, _) = journal(dir.path(), true);
        j.record(&AgentEvent::ThinkingStarted);
        j.record(&AgentEvent::TextDelta("x".into()));
        j.append_if_started(&Record::clear_now());
        assert!(j.path().is_none(), "流式事件与空转指令不应建文件");
        assert_eq!(runtime.read().unwrap().session_id, None);

        j.record(&AgentEvent::MessageAppended(Message::user("你好")));
        let path = j.path().expect("第一条消息后应建文件").to_path_buf();
        assert!(path.is_file());
        let state = runtime.read().unwrap();
        assert_eq!(state.session_id.as_deref(), Some(j.id().unwrap().as_str()));
        assert_eq!(state.session_path.as_deref(), Some(path.as_path()));
    }

    #[test]
    fn ensure_open_creates_the_file_early() {
        let dir = tempfile::tempdir().unwrap();
        let (mut j, runtime, _) = journal(dir.path(), true);
        j.ensure_open();
        assert!(j.path().is_some(), "ensure_open 应立即建文件");
        assert_eq!(
            runtime.read().unwrap().session_id.as_deref(),
            Some(j.id().unwrap().as_str())
        );
        let first = j.path().unwrap().to_path_buf();
        j.ensure_open();
        assert_eq!(j.path().unwrap(), first, "重复调用不应换文件");
    }

    #[test]
    fn ensure_open_is_a_noop_when_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let (mut j, runtime, _) = journal(dir.path(), false);
        j.ensure_open();
        assert!(j.path().is_none());
        assert_eq!(runtime.read().unwrap().session_id, None);
    }

    #[test]
    fn turn_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let (mut j, _runtime, cwd) = journal(dir.path(), true);
        j.record(&AgentEvent::MessageAppended(Message::user("第一问")));
        j.record(&started("c1", "bash", "ls"));
        j.record(&finished("c1", true, "退出码 0", 5));
        j.record(&AgentEvent::MessageAppended(Message::assistant(
            "",
            None,
            vec![seanbot_provider::ToolCall {
                id: "c1".into(),
                name: "bash".into(),
                arguments: "{}".into(),
            }],
        )));
        j.record(&AgentEvent::MessageAppended(Message::tool("c1", "a.txt")));
        j.record(&AgentEvent::MessageAppended(Message::assistant(
            "看完了",
            Some("想".into()),
            Vec::new(),
        )));

        let path = j.path().unwrap().to_path_buf();
        let loaded = session::load_session(&path).unwrap();
        assert_eq!(loaded.meta.cwd, cwd);
        assert_eq!(loaded.meta.system, "系统");
        assert_eq!(loaded.meta.model, "m");
        assert_eq!(
            loaded.history,
            vec![
                Message::user("第一问"),
                Message::assistant(
                    "",
                    None,
                    vec![seanbot_provider::ToolCall {
                        id: "c1".into(),
                        name: "bash".into(),
                        arguments: "{}".into(),
                    }]
                ),
                Message::tool("c1", "a.txt"),
                Message::assistant("看完了", Some("想".into()), Vec::new()),
            ]
        );
        assert_eq!(loaded.tool_ui.len(), 1);
        assert_eq!(loaded.tool_ui["c1"].summary, "退出码 0");
    }

    #[test]
    fn start_new_switches_files_and_clears_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let (mut j, runtime, _) = journal(dir.path(), true);
        j.record(&AgentEvent::MessageAppended(Message::user("旧会话")));
        let first = j.path().unwrap().to_path_buf();

        j.start_new("m2", "新系统提示词");
        assert!(j.path().is_none());
        assert_eq!(runtime.read().unwrap().session_id, None);

        j.record(&AgentEvent::MessageAppended(Message::user("新会话")));
        let second = j.path().unwrap().to_path_buf();
        assert_ne!(first, second);
        assert_eq!(session::load_session(&second).unwrap().meta.model, "m2");
    }

    #[test]
    fn recording_can_be_turned_off() {
        let dir = tempfile::tempdir().unwrap();
        let (mut j, runtime, _) = journal(dir.path(), false);
        j.record(&AgentEvent::MessageAppended(Message::user("你好")));
        assert!(j.path().is_none());
        assert_eq!(runtime.read().unwrap().session_id, None);
        assert!(!j.store().dir_for(&j.cwd).exists());
    }

    #[test]
    fn clear_and_model_land_in_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let (mut j, _runtime, _) = journal(dir.path(), true);
        j.record(&AgentEvent::MessageAppended(Message::user("A")));
        j.record(&AgentEvent::MessageAppended(Message::assistant(
            "a",
            None,
            Vec::new(),
        )));
        j.append(&Record::clear_now());
        j.append(&Record::Model { model: "m3".into() });
        j.record(&AgentEvent::MessageAppended(Message::user("B")));

        let loaded = session::load_session(j.path().unwrap()).unwrap();
        assert_eq!(loaded.history, vec![Message::user("B")]);
        assert_eq!(loaded.model, "m3");
    }
}
