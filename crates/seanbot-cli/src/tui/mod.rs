//! 行内 TUI（设计书 §4）：终端守卫、事件循环、输入框与状态栏。
//!
//! 阶段 2a 只搭骨架：raw mode / bracketed paste / panic 兜底、`Viewport::Inline` 行内界面、
//! 单行输入框、状态栏，以及把一轮对话的事件画在活动区里。斜杠浮窗、确认框、Ctrl+O 转录、
//! 鼠标、环环动画分别在 2d/2e/2f 补上。
//!
//! 阶段 2d 已接上：斜杠浮窗与二级列表（/model、/resume）、/mouse、以及确认模式下 edit/bash 的
//! TUI 确认框（TuiPermission，规则见 permission.rs）。
//!
//! 伪终端在当前开发沙箱里不可用（openpty 被拒），所以界面靠 ratatui TestBackend 断言
//! （活动区/输入行/状态栏/光标），入口与按键逻辑用单元测试覆盖。

mod inline;
mod input;
mod mascot;
mod permission;
mod popup;
mod render;
mod slash;
pub mod terminal;
mod transcript;

use input::Input;
pub use permission::{Ask, AskView, PendingAsks, Rules, TuiPermission};
use popup::{ConfirmState, PickKind, Picker, PickerItem, Popup};
use render::mode_label;
pub use transcript::Entry;

use std::{
    collections::HashMap,
    io::{self},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::Context;
use crossterm::{
    event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    text::Line,
    widgets::{Paragraph, Widget},
};
use seanbot_core::{Agent, AgentEvent, UserEvent, config::Config};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{format, journal::Journal, repl};

/// 动画/重绘节拍。
const TICK: Duration = Duration::from_millis(80);
/// 连按两次 Ctrl+C 退出的时间窗。
const QUIT_WINDOW: Duration = Duration::from_secs(2);

/// 按键处理结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// 什么都不做
    None,
    /// 提交一轮对话
    Submit(String),
    /// 取消正在跑的一轮
    Cancel,
    /// 执行一条斜杠命令
    Command(&'static str),
    /// 二级列表里选中了一项（模型 id / 会话文件路径）
    Choose(PickKind, String),
    /// 打开 Ctrl+O 转录视图
    OpenTranscript,
    /// 退出程序
    Quit,
}

/// 界面状态。
/// 工具调用的显示信息（完整结果要等工具消息回来才有）。
#[derive(Debug, Default, Clone)]
struct ToolMeta {
    name: String,
    title: String,
    ok: bool,
    summary: String,
}

pub struct App {
    input: Input,
    /// 斜杠命令浮窗（输入以 `/` 开头时出现）
    popup: Option<Popup>,
    /// 二级列表（/model、/resume）
    picker: Option<Picker>,
    /// 工具确认框
    confirm: Option<ConfirmState>,
    /// 正在跑的工具（与 TUI 之前的行内渲染一致：标签 + 起始时刻，状态行里转圈）
    running_tool: Option<(String, Instant)>,
    /// 本会话的转录（Ctrl+O）
    transcript: Vec<Entry>,
    /// 工具调用的显示信息，按 call_id 暂存
    tool_meta: HashMap<String, ToolMeta>,
    /// 转录视图的选择/展开状态
    transcript_view: transcript::View,
    /// 欢迎框里的环环
    mascot: mascot::Mascot,
    /// 还没发过消息（欢迎框还留在活动区）
    welcome: bool,
    /// 当前工作目录（状态栏显示用）
    cwd: String,
    /// 待写进终端滚动区的行（写出去就从内存里丢掉）
    /// 已完成输出：行内模式下由 `freeze` 推进终端 scrollback
    pending: Vec<Line<'static>>,
    /// 累计已经交给终端 scrollback 的行数（冻结是"从头部真的取走"，
    /// 不再靠位置游标跳过，避免序列顺序变化时游标错位）
    frozen_total: usize,
    /// 上一帧活动区**实际**画了多少行（冻结策略必须用这个值当上限，否则会丢行）
    active_height: u16,
    /// 历史滚动：0 = 跟随最新，越大越往上看
    scroll: u16,
    /// 本轮助手正文的 Markdown 流式渲染器
    markdown: Option<crate::markdown::Streaming>,
    /// 还没冻结、留在活动区显示的尾部
    tail: Vec<Line<'static>>,
    /// 正文渲染宽度（终端列数）
    width: u16,
    /// 状态栏左侧：模型 · 目录
    left: String,
    /// 状态栏右侧：确认模式 / YOLO
    mode: String,
    /// 一次性提示（版本更新、快捷键提示等）
    hint: Option<String>,
    /// 连按两次 Ctrl+C 的计时
    quit_deadline: Option<Instant>,
    /// UI → 运行时的事件通道（TUI 只发事件：不直接调工具、不直接改内核状态）
    user: mpsc::UnboundedSender<UserEvent>,
    frame: usize,
    running: bool,
}

impl App {
    pub fn new(
        model: &str,
        cwd: &str,
        mode: seanbot_core::PermissionMode,
        user: mpsc::UnboundedSender<UserEvent>,
    ) -> Self {
        let left = format!("{model} · {}", shorten_home(cwd));
        Self {
            input: Input::default(),
            popup: None,
            picker: None,
            confirm: None,
            running_tool: None,
            transcript: Vec::new(),
            tool_meta: HashMap::new(),
            transcript_view: transcript::View::default(),
            mascot: mascot::Mascot::new(mascot::Mood::Idle),
            welcome: true,
            cwd: cwd.to_string(),
            pending: Vec::new(),
            frozen_total: 0,
            active_height: 1,
            scroll: 0,
            markdown: None,
            tail: Vec::new(),
            width: 80,
            left,
            mode: mode_label(mode),
            hint: None,
            quit_deadline: None,
            user,
            frame: 0,
            running: false,
        }
    }

    /// 提交一条输入：**只发事件**，由运行时（主循环）收到后开始这一轮。
    pub fn submit(&mut self, text: String) {
        let _ = self.user.send(UserEvent::Submit(text));
    }

    /// 请求取消正在跑的回合：**只发事件**，真正的取消由运行时持有的
    /// `CancellationToken` 完成（界面拿不到它，也就不可能绕过运行时）。
    pub fn cancel(&mut self) {
        let _ = self.user.send(UserEvent::Cancel);
    }

    /// 回复一次授权询问：**只发事件**（发给谁、怎么回传给内核由运行时决定）。
    ///
    /// `remember` 为真时把规则记进本会话的规则表——记的是界面自己的规则，
    /// 下次同一个工具就不会再弹确认框。
    fn answer(&mut self, allow: bool, remember: bool, reason: Option<String>) {
        let Some(confirm) = self.confirm.take() else {
            return;
        };
        if remember
            && let Some(ask) = confirm.ask.as_ref()
            && let Ok(mut rules) = confirm.rules.lock()
        {
            rules.remember(&ask.request);
        }
        let tool = confirm
            .ask
            .as_ref()
            .map(|ask| ask.tool.clone())
            .unwrap_or_default();
        let _ = self.user.send(UserEvent::PermissionDecision {
            tool,
            allow,
            reason,
        });
    }

    pub fn set_hint(&mut self, hint: String) {
        self.hint = Some(hint);
    }

    /// 处理按键。
    pub fn on_key(&mut self, event: Event) -> Action {
        match event {
            Event::Key(key) => self.on_key_press(key),
            // bracketed paste：整段插入（换行也在文本里），绝不触发提交
            Event::Paste(_) => {
                self.input.apply(event);
                self.refresh_popup();
                Action::None
            }
            _ => Action::None,
        }
    }

    fn on_key_press(&mut self, key: KeyEvent) -> Action {
        // Windows 上会有 Release/Repeat，只认按下
        if key.kind != KeyEventKind::Press {
            return Action::None;
        }
        // 确认框最优先：它挡着内核的一次授权等待
        if self.confirm.is_some() {
            return self.on_confirm_key(key);
        }
        // 二级列表打开时，它优先吃按键
        if self.picker.is_some() {
            match key.code {
                KeyCode::Up => {
                    self.move_picker(-1);
                    return Action::None;
                }
                KeyCode::Down => {
                    self.move_picker(1);
                    return Action::None;
                }
                KeyCode::Enter => {
                    if let Some((kind, value)) = self.selected_picker() {
                        self.picker = None;
                        return Action::Choose(kind, value);
                    }
                }
                KeyCode::Esc => {
                    self.picker = None;
                    return Action::None;
                }
                _ => return Action::None,
            }
        }
        // 浮窗打开时，方向键/Enter/Tab/Esc 先归它
        if self.popup.is_some() {
            match key.code {
                KeyCode::Up => {
                    self.move_selection(-1);
                    return Action::None;
                }
                KeyCode::Down => {
                    self.move_selection(1);
                    return Action::None;
                }
                KeyCode::Tab => {
                    self.complete_selection();
                    return Action::None;
                }
                KeyCode::Esc => {
                    self.popup = None;
                    return Action::None;
                }
                KeyCode::Enter => {
                    if let Some(name) = self.selected_command() {
                        self.popup = None;
                        self.input.clear();
                        if name == "/exit" {
                            return Action::Quit;
                        }
                        return Action::Command(name);
                    }
                }
                _ => {}
            }
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl {
            match key.code {
                KeyCode::Char('c') => return self.on_ctrl_c(),
                KeyCode::Char('d') => {
                    if self.input.is_empty() {
                        return Action::Quit;
                    }
                    return Action::None;
                }
                KeyCode::Char('a') => {
                    self.input.move_home();
                    return Action::None;
                }
                KeyCode::Char('e') => {
                    self.input.move_end();
                    return Action::None;
                }
                KeyCode::Char('u') => {
                    self.input.clear();
                    self.refresh_popup();
                    return Action::None;
                }
                KeyCode::Char('o') => return Action::OpenTranscript,
                _ => return Action::None,
            }
        }
        // 多行输入下 ↑↓ 优先在输入框内移动光标，只有已经在首/末行时才去翻历史
        match key.code {
            KeyCode::Up if !self.input.on_first_line() => {
                self.input.move_line(-1);
                return Action::None;
            }
            KeyCode::Down if !self.input.on_last_line() => {
                self.input.move_line(1);
                return Action::None;
            }
            _ => {}
        }
        match key.code {
            // Enter 是提交；换行要靠 bracketed paste 或 Shift+Enter（见下）
            KeyCode::Enter => {
                let text = self.input.text().trim().to_string();
                if text.is_empty() {
                    return Action::None;
                }
                if text == "/exit" || text == "/quit" {
                    return Action::Quit;
                }
                self.input.take();
                Action::Submit(text)
            }
            KeyCode::Char('\n') => {
                // Shift+Enter / Ctrl+J 这类直接送换行的终端：插入换行而不提交
                self.input.insert_str("\n");
                Action::None
            }
            KeyCode::Char(_)
            | KeyCode::Backspace
            | KeyCode::Delete
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Home
            | KeyCode::End => {
                let event = Event::Key(key);
                self.input.apply(event);
                self.refresh_popup();
                Action::None
            }
            _ => Action::None,
        }
    }

    /// 空闲时 Ctrl+C：有输入先清空，没输入则提示"再按一次退出"。
    fn on_ctrl_c(&mut self) -> Action {
        if self.running {
            // 运行中的中断由事件循环处理（取消当前轮）
            return Action::Cancel;
        }
        if !self.input.is_empty() {
            self.input.clear();
            self.hint = None;
            return Action::None;
        }
        let now = Instant::now();
        match self.quit_deadline {
            Some(deadline) if now < deadline => Action::Quit,
            _ => {
                self.quit_deadline = Some(now + QUIT_WINDOW);
                self.hint = Some("再按一次 Ctrl+C 退出".to_string());
                Action::None
            }
        }
    }

    /// 把内核事件画进活动区。
    pub fn on_agent_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::TextDelta(text) => {
                if self.markdown.is_none() {
                    let width = self.width as usize;
                    self.markdown = Some(crate::markdown::Streaming::new(width));
                }
                if let Some(stream) = self.markdown.as_mut() {
                    // 冻结的行写进滚动区，未完成的尾部留在活动区
                    let frozen = stream.push(&text);
                    self.pending.extend(frozen);
                    self.tail = stream.tail();
                }
            }
            AgentEvent::MessageAppended(message) => self.record_message(message),
            AgentEvent::MessageRetracted => {
                if matches!(self.transcript.last(), Some(Entry::User(_))) {
                    self.transcript.pop();
                }
            }
            AgentEvent::ToolStarted {
                call_id,
                title,
                name,
            } => {
                self.flush_markdown();
                let label = format::tool_label(&name, &title);
                // TUI 之前就是这么显示的：运行中的工具在状态行转圈，不往滚动区塞一行
                self.running_tool = Some((label, Instant::now()));
                self.tool_meta.insert(
                    call_id,
                    ToolMeta {
                        name,
                        title,
                        ok: true,
                        summary: String::new(),
                    },
                );
            }
            AgentEvent::ToolFinished {
                call_id,
                ok,
                summary,
                elapsed,
                ..
            } => {
                let label = self
                    .tool_meta
                    .get(&call_id)
                    .map(|meta| format::tool_label(&meta.name, &meta.title))
                    .unwrap_or_else(|| summary.clone());
                let mark = if ok { "✓" } else { "✗" };
                self.running_tool = None;
                self.push_line(format!("{mark} {label} · {}", format::secs(elapsed)));
                if let Some(meta) = self.tool_meta.get_mut(&call_id) {
                    meta.ok = ok;
                    meta.summary = summary;
                }
            }
            AgentEvent::TurnFinished { usage, steps } => {
                self.mascot.mood = mascot::Mood::Happy;
                self.flush_markdown();
                self.push_line(format::usage_line(usage.as_ref(), steps));
            }
            AgentEvent::Cancelled => {
                self.mascot.mood = mascot::Mood::Error;
                self.flush_markdown();
                self.push_line("⎿ 已中断".to_string());
            }
            AgentEvent::Error(msg) => {
                self.mascot.mood = mascot::Mood::Error;
                self.flush_markdown();
                self.push_line(format!("✗ 错误：{msg}"));
            }
            _ => {}
        }
    }

    /// 一轮开始/结束。
    pub fn turn_started(&mut self, prompt: &str) {
        self.flush_markdown();
        // 第一条消息发出：欢迎框定格为历史的一部分，并且**必须排在最前面**
        // （它此前也是 history_lines 的第一段，位置不能变）
        self.commit_banner();
        self.push_line(format!("› {prompt}"));
        self.transcript.push(Entry::User(prompt.to_string()));
        self.mascot.mood = mascot::Mood::Thinking;
        self.running = true;
    }

    pub fn turn_finished(&mut self) {
        self.flush_markdown();
        // 本轮结束/被取消：确认框随之关掉，内核那边会拿到"拒绝"
        self.confirm = None;
        self.running = false;
    }

    /// 结束本轮 Markdown 流式渲染：剩余内容交给滚动区，活动区清空尾部。
    fn flush_markdown(&mut self) {
        let Some(mut stream) = self.markdown.take() else {
            self.tail.clear();
            return;
        };
        self.pending.extend(stream.finish());
        self.tail.clear();
    }

    fn push_line(&mut self, text: String) {
        self.pending.push(Line::from(text));
    }

    /// 取走待写进滚动区的行。
    #[allow(dead_code)]
    pub fn take_pending(&mut self) -> Vec<Line<'static>> {
        std::mem::take(&mut self.pending)
    }

    /// 终端尺寸变化时更新渲染宽度。
    pub fn set_width(&mut self, width: u16) {
        self.width = width;
    }

    /// 处理滚动相关的输入（鼠标滚轮 / PgUp / PgDn / Home / End）。
    /// 返回 true 表示这个事件已被滚动消费掉。
    pub fn scroll_event(&mut self, event: &Event) -> bool {
        let step = 3u16;
        match event {
            Event::Mouse(mouse) => match mouse.kind {
                crossterm::event::MouseEventKind::ScrollUp => {
                    self.scroll = self.scroll.saturating_add(step)
                }
                crossterm::event::MouseEventKind::ScrollDown => {
                    self.scroll = self.scroll.saturating_sub(step)
                }
                _ => return false,
            },
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::PageUp => self.scroll = self.scroll.saturating_add(10),
                KeyCode::PageDown => self.scroll = self.scroll.saturating_sub(10),
                KeyCode::Home => self.scroll = u16::MAX,
                KeyCode::End => self.scroll = 0,
                _ => return false,
            },
            _ => return false,
        }
        true
    }

    /// 活动区要画的行 = 还没交给终端的行（冻结时已从头部取走，不会再画一遍）。
    ///
    /// 按当前**渲染宽度**折成屏幕行：冻结多少行、窗口取几行都按行数算，
    /// 所以"逻辑行 == 屏幕行"必须在这里保证（见 `inline::wrap_lines`）。
    fn viewport_lines(&self) -> Vec<Line<'static>> {
        inline::wrap_lines(&self.history_lines(), self.width)
    }

    /// 累计交给终端 scrollback 的行数。
    ///
    /// 它同时是"这一轮有没有冻结过东西"的判据：只增不减，所以两次读取之间变了，
    /// 就说明刚有行被写进终端（活动区需要补画一帧）。
    pub fn frozen(&self) -> usize {
        self.frozen_total
    }

    /// 上一帧活动区实际渲染的行数（冻结上限的依据）。
    pub fn active_height(&self) -> u16 {
        self.active_height
    }

    /// 把最前面的 `count` 行从内存里取走（它们已写进终端 scrollback）。
    ///
    /// 这是"冻结"的落地动作：取走之后活动区自然不会再画它们。
    /// 欢迎框如果还没提交，先把它提交进 pending（否则会跳过它直接冻结正文）。
    fn drain_frozen(&mut self, count: usize) {
        let mut count = count;
        if self.welcome && count > 0 {
            self.commit_banner();
        }
        let from_pending = count.min(self.pending.len());
        self.pending.drain(..from_pending);
        count -= from_pending;
        if count > 0 {
            // 还没到 pending 的行只可能是流式尾部：尾部不冻结
            let _ = count;
        }
        self.frozen_total = self.frozen_total.saturating_add(from_pending);
    }

    /// 欢迎框定格为历史的一部分，并排在 pending 最前面。
    fn commit_banner(&mut self) {
        if !self.welcome {
            return;
        }
        self.welcome = false;
        let banner = self.banner();
        self.pending.splice(0..0, banner);
    }

    /// 历史区内容：欢迎框（首屏）+ 已完成输出 + 还没冻结的流式尾部。
    ///
    /// **顺序必须是 append-only 的**：`frozen` 是这份序列的位置前缀游标，
    /// 任何"整体前移/后移"都会让游标错位（把没冻结过的行当成已冻结而永不显示）。
    /// 所以欢迎框固定在**最前面**：`turn_started` 提交它时只是把它从
    /// "首屏临时行" 变成 `pending` 的第一段，位置不变。
    fn history_lines(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        if self.welcome {
            lines.extend(self.banner());
        }
        lines.extend(self.pending.iter().cloned());
        lines.extend(self.tail.iter().cloned());
        lines
    }

    /// 活动区视图的行：默认贴底（显示最新内容），向上滚动由 `scroll` 控制。
    fn viewport_window(&self, height: usize) -> Vec<Line<'static>> {
        let lines = self.viewport_lines();
        let hidden = lines.len().saturating_sub(height).min(u16::MAX as usize) as u16;
        let offset = hidden.saturating_sub(self.scroll) as usize;
        lines.into_iter().skip(offset).take(height).collect()
    }

    pub fn tick(&mut self) {
        self.frame = self.frame.wrapping_add(1);
        self.mascot.tick();
    }

    /// 欢迎框（沿用当前帧，冻结进滚动区时也是这一帧）。
    fn banner(&self) -> Vec<Line<'static>> {
        mascot::welcome_lines(
            &self.mascot,
            env!("CARGO_PKG_VERSION"),
            &self.model_name(),
            &self.mode,
            &shorten_home(&self.cwd),
            self.width,
        )
    }

    /// 状态栏左侧里显示的模型名（从 `模型 · 目录` 里取回模型部分）。
    fn model_name(&self) -> String {
        self.left
            .split(" · ")
            .next()
            .unwrap_or_default()
            .to_string()
    }

    /// 转录内容（Ctrl+O 视图用）。
    pub(crate) fn transcript(&self) -> &[Entry] {
        &self.transcript
    }

    pub(crate) fn transcript_view(&self) -> &transcript::View {
        &self.transcript_view
    }

    pub(crate) fn transcript_view_mut(&mut self) -> &mut transcript::View {
        &mut self.transcript_view
    }

    /// 从内核消息里补转录：工具结果的完整内容只在这里有。
    fn record_message(&mut self, message: seanbot_provider::Message) {
        match message.role {
            seanbot_provider::Role::Tool => {
                let id = message.tool_call_id.clone().unwrap_or_default();
                let meta = self.tool_meta.remove(&id).unwrap_or_default();
                self.transcript.push(Entry::Tool {
                    name: meta.name,
                    title: meta.title,
                    ok: meta.ok,
                    summary: meta.summary,
                    content: message.content,
                });
            }
            seanbot_provider::Role::Assistant => {
                if message.content.trim().is_empty() {
                    return;
                }
                // 流式期间可能已经记了一条，用最终内容替换
                if matches!(self.transcript.last(), Some(Entry::Assistant(_))) {
                    self.transcript.pop();
                }
                self.transcript.push(Entry::Assistant(message.content));
            }
            seanbot_provider::Role::User => {
                // turn_started 已经记过一次，别重复
                if matches!(self.transcript.last(), Some(Entry::User(text)) if *text == message.content)
                {
                    return;
                }
                self.transcript.push(Entry::User(message.content));
            }
        }
    }

    /// 内核请求授权：打开确认框。
    pub fn open_confirm(&mut self, ask: AskView, rules: Arc<Mutex<Rules>>) {
        self.confirm = Some(ConfirmState {
            ask: Some(ask),
            selected: 0,
            reason: None,
            rules,
        });
    }

    /// 确认框按键。
    fn on_confirm_key(&mut self, key: KeyEvent) -> Action {
        // 正在写拒绝原因
        let writing = self
            .confirm
            .as_ref()
            .is_some_and(|confirm| confirm.reason.is_some());
        if writing {
            match key.code {
                KeyCode::Enter => {
                    let text = self
                        .confirm
                        .as_ref()
                        .and_then(|confirm| confirm.reason.as_ref())
                        .map(|reason| reason.text().trim().to_string())
                        .unwrap_or_default();
                    let reason = if text.is_empty() { None } else { Some(text) };
                    self.answer(false, false, reason);
                }
                KeyCode::Esc => {
                    if let Some(confirm) = self.confirm.as_mut() {
                        confirm.reason = None;
                    }
                }
                _ => {
                    if let Some(reason) = self
                        .confirm
                        .as_mut()
                        .and_then(|confirm| confirm.reason.as_mut())
                    {
                        // 拒绝原因也是 tui-textarea：字符/退格/删除/左右都由它处理
                        reason.apply(Event::Key(key));
                    }
                }
            }
            return Action::None;
        }

        let options = self
            .confirm
            .as_ref()
            .map(|confirm| confirm.options().len())
            .unwrap_or(0);
        if options == 0 {
            return Action::None;
        }
        match key.code {
            KeyCode::Up => {
                if let Some(confirm) = self.confirm.as_mut() {
                    confirm.selected = (confirm.selected + options - 1) % options;
                }
            }
            KeyCode::Down => {
                if let Some(confirm) = self.confirm.as_mut() {
                    confirm.selected = (confirm.selected + 1) % options;
                }
            }
            KeyCode::Char(digit @ '1'..='3') => {
                let index = digit as usize - '1' as usize;
                if index < options {
                    if let Some(confirm) = self.confirm.as_mut() {
                        confirm.selected = index;
                    }
                    self.apply_confirm();
                }
            }
            KeyCode::Enter => self.apply_confirm(),
            KeyCode::Esc => self.answer(false, false, None),
            _ => {}
        }
        Action::None
    }

    /// 执行确认框里选中的选项。
    fn apply_confirm(&mut self) {
        let Some(confirm) = self.confirm.as_ref() else {
            return;
        };
        let selected = confirm.selected;
        let last = confirm.options().len() - 1;
        if selected == last {
            // 第三项：展开原因输入框
            if let Some(confirm) = self.confirm.as_mut() {
                confirm.reason = Some(Input::default());
            }
            return;
        }
        // 第一项是"允许"，第二项是"允许，本会话不再询问"
        self.answer(true, selected == 1, None);
    }

    /// 打开二级列表（没有候选项时直接在滚动区说明）。
    fn open_picker(&mut self, kind: PickKind, title: &str, items: Vec<PickerItem>) {
        if items.is_empty() {
            self.push_line(format!("{title}：没有可选项"));
            return;
        }
        self.picker = Some(Picker {
            title: title.to_string(),
            kind,
            items,
            selected: 0,
        });
    }

    /// 列表当前选中项（类型 + 值）。
    fn selected_picker(&self) -> Option<(PickKind, String)> {
        let picker = self.picker.as_ref()?;
        let item = picker.items.get(picker.selected)?;
        Some((picker.kind, item.value.clone()))
    }

    /// 上下移动列表选中项（循环）。
    fn move_picker(&mut self, delta: isize) {
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        let len = picker.items.len() as isize;
        if len == 0 {
            return;
        }
        picker.selected = (picker.selected as isize + delta).rem_euclid(len) as usize;
    }

    /// 切换模型后刷新状态栏。
    pub fn set_model(&mut self, model: &str) {
        self.left = format!("{model} · {}", shorten_home(&self.cwd));
    }

    /// 浮窗当前选中的命令名。
    fn selected_command(&self) -> Option<&'static str> {
        let popup = self.popup.as_ref()?;
        let item = popup.matches.get(popup.selected)?;
        Some(slash::COMMANDS[item.index].name)
    }

    /// 输入变化后刷新浮窗（以 / 开头且还没有空格时才显示）。
    fn refresh_popup(&mut self) {
        let text = self.input.text();
        let Some(query) = slash::popup_query(&text) else {
            self.popup = None;
            return;
        };
        let matches = slash::filter(query);
        if matches.is_empty() {
            self.popup = None;
            return;
        }
        let selected = self
            .popup
            .as_ref()
            .map(|popup| popup.selected.min(matches.len() - 1))
            .unwrap_or(0);
        self.popup = Some(Popup { matches, selected });
    }

    /// 上下移动浮窗选中项（循环）。
    fn move_selection(&mut self, delta: isize) {
        let Some(popup) = self.popup.as_mut() else {
            return;
        };
        let len = popup.matches.len() as isize;
        if len == 0 {
            return;
        }
        popup.selected = (popup.selected as isize + delta).rem_euclid(len) as usize;
    }

    /// Tab 补全：把选中的命令名填进输入框。
    fn complete_selection(&mut self) {
        let Some(name) = self.selected_command() else {
            return;
        };
        self.input.clear();
        self.input.insert_str(name);
        self.refresh_popup();
    }

    /// 权限模式变化后更新状态栏。
    pub fn set_mode(&mut self, mode: seanbot_core::PermissionMode) {
        self.mode = mode_label(mode);
    }
}

/// 二级列表选完之后落地：切模型或恢复会话。
fn apply_choice(
    kind: PickKind,
    value: String,
    agent: &mut Agent,
    journal: &Arc<Mutex<Journal>>,
    app: &mut App,
) {
    match kind {
        PickKind::Model => {
            agent.set_model(value.clone());
            if let Ok(mut journal) = journal.lock() {
                journal.append_if_started(&seanbot_core::session::Record::Model {
                    model: value.clone(),
                });
            }
            app.set_model(&value);
            app.push_line(format!("已切换模型：{value}"));
        }
        PickKind::Session => {
            match seanbot_core::session::resume_session(std::path::Path::new(&value)) {
                Ok((loaded, writer)) => {
                    agent.restore(loaded.meta.system.clone(), loaded.history.clone());
                    agent.set_model(loaded.model.clone());
                    if let Ok(mut journal) = journal.lock() {
                        journal.bind(loaded.meta.clone(), writer);
                    }
                    app.set_model(&loaded.model);
                    app.push_line(format!(
                        "已恢复会话 {}（{} 条消息）",
                        loaded.meta.id,
                        loaded.history.len()
                    ));
                    for warning in &loaded.warnings {
                        app.push_line(format!("提示：{warning}"));
                    }
                }
                Err(e) => app.push_line(format!("恢复会话失败：{e}")),
            }
        }
    }
}

/// 执行斜杠命令。阶段 2d 先接上不需要列表界面的几条，/model 与 /resume 的列表在下一步。
async fn run_command(
    name: &str,
    agent: &mut Agent,
    journal: &Arc<Mutex<Journal>>,
    cwd: &str,
    app: &mut App,
    cfg: &Config,
) {
    match name {
        "/help" => {
            for line in repl::HELP.lines() {
                app.push_line(line.to_string());
            }
        }
        "/clear" => {
            agent.clear();
            if let Ok(mut journal) = journal.lock() {
                journal.append_if_started(&seanbot_core::session::Record::clear_now());
            }
            app.push_line("已清空对话".to_string());
        }
        "/yolo" => {
            repl::toggle_permission_mode(agent);
            let mode = agent
                .runtime()
                .read()
                .map(|state| state.permission_mode)
                .unwrap_or(seanbot_core::PermissionMode::Confirm);
            app.set_mode(mode);
            app.push_line(format!("权限模式：{}", mode_label(mode)));
        }
        "/new" => {
            repl::start_new(agent, journal, std::path::Path::new(cwd));
            app.push_line("已开始新会话".to_string());
        }
        "/model" => match agent.provider().list_models().await {
            Ok(models) => {
                let current = agent.model().to_string();
                let items: Vec<PickerItem> = models
                    .iter()
                    .map(|model| PickerItem {
                        label: format!(
                            "{}{}",
                            if model.id == current { "● " } else { "  " },
                            model.id
                        ),
                        detail: format!(
                            "{}k 上下文{}",
                            model.context_window / 1000,
                            if model.supports_tools {
                                " · 支持工具"
                            } else {
                                ""
                            }
                        ),
                        value: model.id.clone(),
                    })
                    .collect();
                app.open_picker(PickKind::Model, "切换模型", items);
            }
            Err(e) => app.push_line(format!("获取模型列表失败：{e}")),
        },
        "/resume" => {
            let listed = match seanbot_core::session::SessionStore::open_default() {
                Ok(store) => store.list(std::path::Path::new(cwd)),
                Err(e) => Err(e),
            };
            match listed {
                Ok(sessions) if sessions.is_empty() => {
                    app.push_line("当前目录没有历史会话".to_string());
                }
                Ok(sessions) => {
                    let items: Vec<PickerItem> = sessions
                        .iter()
                        .map(|session| {
                            let first = session.first_prompt.lines().next().unwrap_or_default();
                            let short: String = first.chars().take(40).collect();
                            PickerItem {
                                label: if short.is_empty() {
                                    session.id.clone()
                                } else {
                                    short
                                },
                                detail: format!("{} 条消息 · {}", session.messages, session.id),
                                value: session.path.display().to_string(),
                            }
                        })
                        .collect();
                    app.open_picker(PickKind::Session, "恢复会话", items);
                }
                Err(e) => app.push_line(format!("列出会话失败：{e}")),
            }
        }
        "/mouse" => {
            let mut updated = cfg.clone();
            updated.ui.mouse = !cfg.ui.mouse;
            let saved = seanbot_core::config::config_path()
                .ok()
                .is_some_and(|path| updated.save_to(&path).is_ok());
            if !saved {
                app.push_line("鼠标开关未能写回配置".to_string());
            } else {
                let enabled = updated.ui.mouse;
                let result = if enabled {
                    execute!(io::stdout(), crossterm::event::EnableMouseCapture)
                } else {
                    execute!(io::stdout(), crossterm::event::DisableMouseCapture)
                };
                match result {
                    Ok(()) => app.push_line(format!(
                        "鼠标支持：{}（开启后按住 Shift 才能选择文本）",
                        if enabled { "开" } else { "关" }
                    )),
                    Err(e) => app.push_line(format!("切换鼠标支持失败：{e}")),
                }
            }
        }
        other => app.push_line(format!("未知命令：{other}")),
    }
}

/// 主目录用 `~` 缩写，状态栏不至于太长。
fn shorten_home(cwd: &str) -> String {
    match dirs::home_dir() {
        Some(home) => match cwd.strip_prefix(&home.to_string_lossy().to_string()) {
            Some(rest) => format!("~{rest}"),
            None => cwd.to_string(),
        },
        None => cwd.to_string(),
    }
}

/// 取一个待处理的终端事件：**在当前线程**上读。
///
/// 行内视口的 `insert_before` 要读 CPR（光标位置查询）的回包，也是在当前线程上读；
/// 一旦再开一个读取者（crossterm 的 `EventStream` 会起读线程），两者就抢同一个终端输入，
/// CPR 读超时——用户看到的 `The cursor position could not be read` 就是这么来的。
fn poll_event() -> Option<Event> {
    if !crossterm::event::poll(Duration::ZERO).unwrap_or(false) {
        return None;
    }
    crossterm::event::read().ok()
}

/// 启动 TUI。返回码沿用 CLI 约定。
pub async fn run(
    agent: &mut Agent,
    cfg: &Config,
    journal: &Arc<Mutex<Journal>>,
    mut hint_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
    mut perm_rx: mpsc::UnboundedReceiver<Ask>,
    rules: Arc<Mutex<Rules>>,
) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()
        .unwrap_or_default()
        .display()
        .to_string();
    let mode = agent
        .runtime()
        .read()
        .map(|state| state.permission_mode)
        .unwrap_or(seanbot_core::PermissionMode::Confirm);
    // UI → 运行时的事件通道：界面只发事件，运行时（主循环）负责落内核动作
    let (user_tx, mut user_rx) = mpsc::unbounded_channel::<UserEvent>();
    let mut app = App::new(agent.model(), &cwd, mode, user_tx);

    // Inline TUI：`Viewport::Inline` 才能用 `insert_before` 把已完成输出冻结进终端
    // scrollback（Fullscreen/Fixed 视口下 insert_before 是空操作）。
    let options = terminal::Options {
        mouse: cfg.ui.mouse,
    };
    let (_, screen_rows) = crossterm::terminal::size().unwrap_or((80, 24));
    // 活动区上限：屏幕的一半（给 scrollback 留地方）
    let reserved =
        (screen_rows / 2).clamp(inline::MIN_WINDOW + 2, screen_rows.saturating_sub(2).max(6));
    // 行内视口要读 CPR（光标位置查询）来确定锚点：终端不回应（裸 pty、脚本/某些复用器）
    // 时这里会超时——**不能让整个 Agent 因此不可用**，退回备用屏幕继续跑。
    // `Session::enter` 失败时它内部的守卫已经还原过终端，所以这里可以安全降级。
    let mut session = match terminal::Session::enter(reserved, options) {
        Ok(session) => session,
        Err(e) => {
            eprintln!("行内界面不可用（{e}），本次改用全屏界面");
            None
        }
    };
    // 不是 TTY（管道/CI）或者行内视口起不来：退回备用屏幕
    let mut fallback = if session.is_none() {
        terminal::enter_alternate().context("进入备用屏幕失败")?;
        let _guard = terminal::Guard::enter(options).context("进入终端原始模式失败")?;
        Some((_guard, Terminal::new(CrosstermBackend::new(io::stdout()))?))
    } else {
        None
    };
    let mut window = inline::Frozen::new(reserved, 80);
    let mut ticker = tokio::time::interval(TICK);

    /// 退出：行内模式交付完整 scrollback，兜底模式退回备用屏幕。
    fn leave(session: &mut Option<terminal::Session>, fallback: &mut bool) -> anyhow::Result<()> {
        if let Some(session) = session.as_mut() {
            session.finish()?;
        }
        if *fallback {
            let _ = terminal::leave_alternate();
            *fallback = false;
        }
        Ok(())
    }

    loop {
        // 每次重绘前：更新宽度、按内容调整活动区高度、把冻结内容写进滚动区
        let (cols, _rows) = crossterm::terminal::size().unwrap_or((80, 24));
        app.set_width(cols);
        window.resize_width(cols);
        render(&mut window, &mut app, cols, &mut session, &mut fallback)?;
        // 提示状态：等界面投来的 `Submit`（界面自己不会直接开始一轮）
        let prompt = loop {
            // 按键在本线程处理（与 CPR 查询同一个线程，避免两个读取者）
            let mut quit = false;
            while let Some(event) = poll_event() {
                if app.scroll_event(&event) {
                    continue;
                }
                match app.on_key(event) {
                    // 提交与取消都走事件通道：界面只发事件，运行时才动内核
                    Action::Submit(text) => app.submit(text),
                    Action::Quit => {
                        quit = true;
                        break;
                    }
                    Action::Command(name) => {
                        run_command(name, agent, journal, &cwd, &mut app, cfg).await
                    }
                    Action::OpenTranscript => match session.as_mut() {
                        Some(session) => {
                            transcript::show(session.terminal(), &mut app, cfg.ui.mouse).await?
                        }
                        None => {
                            if let Some((_, terminal)) = fallback.as_mut() {
                                transcript::show(terminal, &mut app, cfg.ui.mouse).await?;
                            }
                        }
                    },
                    Action::Choose(kind, value) => {
                        apply_choice(kind, value, agent, journal, &mut app)
                    }
                    _ => {}
                }
            }
            if quit {
                let mut has_fallback = fallback.is_some();
                leave(&mut session, &mut has_fallback)?;
                return Ok(());
            }
            let mut prompt = None;
            tokio::select! {
                Some(hint) = hint_rx.recv() => app.set_hint(hint),
                Some(event) = user_rx.recv() => match event {
                    UserEvent::Submit(text) => prompt = Some(text),
                    // 没人在跑回合：取消无事可做；授权答复同理（没有待答复的询问）
                    UserEvent::Cancel | UserEvent::PermissionDecision { .. } => {}
                },
                _ = ticker.tick() => app.tick(),
            }
            render(&mut window, &mut app, cols, &mut session, &mut fallback)?;
            if let Some(prompt) = prompt {
                break prompt;
            }
        };

        // 跑一轮：界面继续响应按键，Esc / Ctrl+C 发 `Cancel` 事件
        let (tx, mut rx) = mpsc::channel(256);
        if let Ok(mut journal) = journal.lock() {
            journal.ensure_open();
        }
        let cancel = CancellationToken::new();
        // 内核正在等哪几次授权：回传通道只在这里，界面拿不到
        let mut pending = PendingAsks::default();
        app.turn_started(&prompt);
        let mut turn = std::pin::pin!(agent.run_turn(prompt, tx, cancel.clone()));
        loop {
            while let Some(event) = poll_event() {
                if app.scroll_event(&event) {
                    continue;
                }
                match app.on_key(event) {
                    Action::Cancel | Action::Quit => {
                        if app.running {
                            app.cancel();
                        } else {
                            let mut has_fallback = fallback.is_some();
                            leave(&mut session, &mut has_fallback)?;
                            return Ok(());
                        }
                    }
                    _ => {}
                }
            }
            render(&mut window, &mut app, cols, &mut session, &mut fallback)?;
            tokio::select! {
                result = &mut turn => {
                    if let Err(e) = result {
                        app.on_agent_event(AgentEvent::Error(e.to_string()));
                    }
                    app.turn_finished();
                    break;
                }
                Some(event) = rx.recv() => {
                    if let Ok(mut journal) = journal.lock() {
                        journal.record(&event);
                    }
                    app.on_agent_event(event);
                }
                Some(ask) = perm_rx.recv() => {
                    // 内核在等这次授权：回传通道留下，界面只拿显示用的部分去弹确认框
                    let (reply, view) = ask.split();
                    pending.push(view.tool.clone(), reply);
                    app.open_confirm(view, rules.clone());
                }
                Some(event) = user_rx.recv() => match event {
                    UserEvent::Cancel => cancel.cancel(),
                    UserEvent::PermissionDecision { .. } => {
                        // 按工具名配对；没有对应询问就忽略（比如上一轮的答复迟到了）
                        pending.resolve(&event);
                    }
                    // 一轮还没跑完：不接受新提交（与旧行为一致）
                    UserEvent::Submit(_) => {}
                },
                _ = ticker.tick() => app.tick(),
            }
        }
        // 本轮结束：还没答复的授权随名单一起丢弃 → 内核按拒绝处理
        drop(pending);
    }
}

/// 画一帧：先渲染（`App` 会记录活动区实际行数），再按这个真实高度冻结前缀。
///
/// 冻结只有在真的写出去之后才需要重画一帧（活动区少了一部分行），所以用
/// `freezes()` 计数器判断，避免每帧都白画两次。
fn render(
    window: &mut inline::Frozen,
    app: &mut App,
    cols: u16,
    session: &mut Option<terminal::Session>,
    fallback: &mut Option<(terminal::Guard, Terminal<CrosstermBackend<io::Stdout>>)>,
) -> anyhow::Result<()> {
    match session.as_mut() {
        Some(session) => {
            snapshot(window, app, cols, session.terminal())?;
        }
        None => {
            if let Some((_, terminal)) = fallback.as_mut() {
                terminal.draw(|frame| app.draw(frame))?;
            }
        }
    }
    Ok(())
}

/// 画一帧 -> 用**实际活动区高度**同步冻结上限 -> 冻结前缀。
///
/// 冻结只有在真的写出去之后才需要补画一帧（活动区少了一部分行），所以用单调的
/// `frozen()` 游标判断有没有冻过东西，避免每帧都白画两次。
fn snapshot<B: ratatui::backend::Backend>(
    window: &mut inline::Frozen,
    app: &mut App,
    cols: u16,
    terminal: &mut Terminal<B>,
) -> anyhow::Result<()> {
    terminal.draw(|frame| app.draw(frame))?;
    window.sync_draw_height(app.active_height());
    let before = app.frozen();
    freeze(window, app, cols, terminal)?;
    if app.frozen() != before {
        // 冻结掉了若干行：活动区内容变了，补画一帧
        terminal.draw(|frame| app.draw(frame))?;
    }
    Ok(())
}

/// 把"已完成输出"推进终端 scrollback。
///
/// 这是 Inline TUI 的核心：`insert_before` 让已完成的行滚进终端历史，而不是留在
/// 活动区重绘。冻结是单调的——`app.frozen()` 只增不减，同一行永远不会插入两次。
fn freeze<B: ratatui::backend::Backend>(
    window: &mut inline::Frozen,
    app: &mut App,
    width: u16,
    terminal: &mut Terminal<B>,
) -> anyhow::Result<()> {
    // 视口宽度用终端实际宽度（冻结出去的行要铺满整行）
    let viewport_width = terminal.get_frame().area().width.max(1).max(width);
    window.resize_width(viewport_width);

    // 要冻结的行 = 超出活动区上限的那部分前缀
    let lines = app.viewport_lines();
    let count = window.freeze_count(lines.len());
    if count == 0 {
        return Ok(());
    }
    // 这里**不需要**在写完之后整屏重画：`insert_before` 要么把冻结行画在视口上方
    // （视口不动），要么把整个视口连同内容一起区域滚动下去（视口下移），两种情况下
    // "屏幕上视口那几行"都等于上一帧的缓冲，ratatui 的增量重绘依然对得上。
    // 视口下移时唯一"看起来会错位"的地方是视口上方的空行，而那几行正是刚写的冻结行。
    for line in lines[..count].iter() {
        let owned = inline::pad_line(line, viewport_width);
        terminal
            .insert_before(1, |buf| {
                let area = buf.area;
                Paragraph::new(owned.clone()).render(area, buf);
            })
            .context("写入终端 scrollback 失败")?;
    }
    // 写进终端之后就从内存里真的取走：活动区不会再画一遍
    app.drain_frozen(count);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    /// 测试用的 App + 事件通道的接收端：界面发出去的 `UserEvent` 都能在这里断言。
    fn app_with_events() -> (App, mpsc::UnboundedReceiver<UserEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let app = App::new(
            "deepseek-flash",
            "/tmp/proj",
            seanbot_core::PermissionMode::Confirm,
            tx,
        );
        (app, rx)
    }

    fn new_app() -> App {
        app_with_events().0
    }

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn ctrl(c: char) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    /// 冻结之后，屏幕上的视口内容必须与 App 认定的窗口**严格一致**：
    /// 既不能有旧内容残留，也不能少行（少行就是"看起来像丢了内容"）。
    #[test]
    fn the_active_viewport_shows_exactly_the_window_after_freezing() {
        let (mut terminal, mut window) = inline_fixture(40, 12, 5);
        let mut app = new_app();
        app.welcome = false;
        for i in 0..8 {
            app.push_line(format!("L-{i:02}"));
        }

        inline_terminal(&mut window, &mut app, 40, &mut terminal);

        // 8 行内容、活动区上限 3 行 → 前 5 行冻结进上面的区域
        assert_eq!(app.frozen(), 5, "冻结的应当是前 5 行");
        let rows = screen_rows(&terminal);
        for i in 0..5 {
            assert_eq!(rows[i], format!("L-{i:02}"), "冻结行错位：{rows:?}");
        }
        // 视口里：窗口最后 3 行 + 输入行 + 状态行
        let area = terminal.get_frame().area();
        let viewport = &rows[area.y as usize..area.bottom() as usize];
        assert_eq!(
            &viewport[..3],
            &["L-05".to_string(), "L-06".into(), "L-07".into()],
            "视口内容与窗口对不上（旧内容残留或少行）：{rows:?}"
        );
        assert!(
            viewport[3].starts_with("› "),
            "倒数第二行应当是输入行：{viewport:?}"
        );
        assert!(
            viewport[4].contains("deepseek-flash"),
            "最后一行应当是状态栏：{viewport:?}"
        );
    }

    /// 窄终端里欢迎框必须完全装得下（不能折行，折行就会顶掉活动区的最后一行）。
    #[test]
    fn the_welcome_box_fits_a_narrow_terminal_without_wrapping() {
        let (mut terminal, mut window) = inline_fixture(40, 12, 5);
        let mut app = new_app();
        assert!(app.welcome);
        inline_terminal(&mut window, &mut app, 40, &mut terminal);

        // 活动区里画出来的每一行都必须是完整的欢迎框行：右边框得在（没被折掉）
        let rows = screen_rows(&terminal);
        let visible: Vec<&String> = rows.iter().filter(|row| row.contains('│')).collect();
        assert!(!visible.is_empty(), "欢迎框应当可见：{rows:?}");
        for row in &visible {
            assert!(
                row.trim_end().ends_with('│') || row.trim_end().ends_with('┘'),
                "欢迎框的右边框不该被折掉：{row:?}"
            );
        }
    }

    /// P0 回归：欢迎框还在活动区时就把首条消息发出去，不能丢消息、也不能重复欢迎框。
    #[test]
    fn first_message_is_not_lost_while_the_welcome_box_is_still_frozen() {
        // 矮终端：行内视口小，欢迎框（8 行）放不下，会被部分冻结
        let (mut terminal, mut window) = inline_fixture(40, 12, 5);
        let mut app = new_app();
        assert!(app.welcome, "开局应当是欢迎状态");
        let banner_len = app.banner().len();
        assert!(banner_len > 3, "欢迎框应当有多行: {banner_len}");

        // 开局先冻结一次（模拟进入 TUI 后的第一帧）
        inline_terminal(&mut window, &mut app, 40, &mut terminal);
        assert!(app.frozen() > 0, "欢迎框应当有一部分被冻结");
        assert!(app.frozen() < banner_len, "欢迎框不该被全部冻结");

        // 第一条消息：欢迎框从活动区挪进 pending，prompt 插在它前面
        app.turn_started("第一条消息");

        // 冻结前缀已经被真的取走：剩下的行里必须还在，不能凭空少
        let remaining = lines_text(&app.viewport_lines());
        assert!(!remaining.is_empty(), "冻结后活动区不该空");

        terminal.draw(|frame| app.draw(frame)).unwrap();
        let rendered = rendered_rows(&terminal).join("\n");
        assert!(
            rendered.contains("第一条消息"),
            "首条用户消息丢了：\n{rendered}"
        );

        // 欢迎框的行不能同时出现在两个位置
        let banner = app.banner();
        let first_banner = lines_text(&banner[..1]);
        let occurrences = rendered.matches(first_banner.trim()).count();
        assert!(
            occurrences <= 1,
            "欢迎框重复渲染了 {occurrences} 次：\n{rendered}"
        );
    }

    /// 把一组行拼成纯文本（断言内容是否还在用）。
    fn lines_text(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    // ---- Phase 4：streaming 与冻结的配合 ----

    #[test]
    fn streamed_content_survives_tool_interruption() {
        // 真实的一轮：正文流到一半 → 触发工具 → 工具结束 → 正文继续 → 收尾
        let mut app = new_app();
        app.turn_started("看看仓库");
        app.on_agent_event(AgentEvent::TextDelta("正在分析".into()));
        let after_first = format!("{}\n{}", lines_text(&app.pending), lines_text(&app.tail));
        assert!(after_first.contains("正在分析"), "{after_first:?}");

        // 工具打断：flush_markdown 会把尾部收进 pending，不能丢内容
        app.on_agent_event(AgentEvent::ToolStarted {
            call_id: "c1".into(),
            name: "bash".into(),
            title: "cargo test".into(),
        });
        let after_tool = format!("{}\n{}", lines_text(&app.pending), lines_text(&app.tail));
        assert!(
            after_tool.contains("正在分析"),
            "工具打断后正文丢了：{after_tool:?}"
        );

        app.on_agent_event(AgentEvent::ToolFinished {
            call_id: "c1".into(),
            ok: true,
            summary: "通过".into(),
            preview: Vec::new(),
            elapsed: Duration::from_millis(10),
        });
        app.on_agent_event(AgentEvent::TextDelta("继续分析".into()));
        app.turn_finished();

        let everything = format!("{}\n{}", lines_text(&app.pending), lines_text(&app.tail));
        assert!(everything.contains("正在分析"), "{everything:?}");
        assert!(everything.contains("继续分析"), "{everything:?}");
    }

    #[test]
    fn streaming_tail_stays_out_of_the_frozen_prefix() {
        // 关键不变式：正在流式的尾部永远不能被当作"已完成"冻结出去
        let mut app = new_app();
        app.welcome = false;
        app.turn_started("问题");
        for i in 0..40 {
            app.on_agent_event(AgentEvent::TextDelta(format!("第{i}行内容\n")));
        }
        let total = app.history_lines().len();
        let frozen = app.frozen();
        assert!(frozen <= total.saturating_sub(1), "冻结不能吃掉全部历史");
        // 最少要留下一行给活动区（流式尾部）
        assert!(total - frozen >= 1, "frozen={frozen} total={total}");
        // 尾部（没冻结的部分）必须还在，而且包含最新的内容
        let visible = lines_text(&app.viewport_lines());
        assert!(visible.contains("第39行内容"), "{visible:?}");
    }

    // ---- Inline TUI（Phase 2）：冻结进终端 scrollback 的行为 ----

    /// 走真实渲染路径（画一帧 -> 同步活动区高度 -> 冻结）的行内终端。
    fn inline_terminal(
        window: &mut inline::Frozen,
        app: &mut App,
        cols: u16,
        terminal: &mut Terminal<ratatui::backend::TestBackend>,
    ) {
        snapshot(window, app, cols, terminal).unwrap();
    }

    /// 走真实 freeze 路径的行内终端 + 窗口状态。
    fn inline_fixture(
        width: u16,
        height: u16,
        rows: u16,
    ) -> (Terminal<ratatui::backend::TestBackend>, inline::Frozen) {
        let terminal = terminal::test_terminal(width, height, rows);
        (terminal, inline::Frozen::new(rows, width))
    }

    /// 把终端 scrollback 的每一行读成字符串（trim 掉行尾补位空格）。
    fn scrollback_rows(terminal: &Terminal<ratatui::backend::TestBackend>) -> Vec<String> {
        let sb = terminal.backend().scrollback();
        (0..sb.area.height).map(|y| row_text(sb, y)).collect()
    }

    /// 屏幕上渲染出来的全部行（含"已冻结但在屏幕上还看得见"的那些）。
    fn rendered_rows(terminal: &Terminal<ratatui::backend::TestBackend>) -> Vec<String> {
        scrollback_rows(terminal)
            .into_iter()
            .chain(screen_rows(terminal))
            .collect()
    }

    fn screen_rows(terminal: &Terminal<ratatui::backend::TestBackend>) -> Vec<String> {
        let buf = terminal.backend().buffer();
        (0..buf.area.height).map(|y| row_text(buf, y)).collect()
    }

    /// 按**终端的方式**读一行：宽字符（CJK）占两格，其后那一格是续格。
    ///
    /// 直接逐格取 `symbol()` 会把续格里的旧内容也读出来（增量重绘不会覆盖续格），
    /// 于是 "第一条" 会被读成 "第─一─条─"。这里按显示宽度前进，等价于终端的排版。
    fn row_text(buf: &ratatui::buffer::Buffer, y: u16) -> String {
        let mut out = String::new();
        let mut x = 0u16;
        while x < buf.area.width {
            let symbol = buf[(x, y)].symbol();
            out.push_str(symbol);
            x += unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
        }
        out.trim_end().to_string()
    }

    #[test]
    fn frozen_history_lands_in_terminal_scrollback() {
        let (mut terminal, mut window) = inline_fixture(40, 20, 5);
        let mut app = new_app();
        // 造 30 行已完成输出：视口放不下的部分必须冻结出去
        for i in 0..30 {
            app.push_line(format!("MESSAGE-{i:02}"));
        }
        app.welcome = false;
        assert_eq!(app.history_lines().len(), 30);

        inline_terminal(&mut window, &mut app, 40, &mut terminal);
        terminal.draw(|frame| app.draw(frame)).unwrap();

        // 冻结的行数 == 活动区放不下的行数
        let frozen = app.frozen();
        assert_eq!(frozen, window.freeze_count(30));
        assert!(frozen > 0, "30 行内容必须有一部分冻结出去");
        assert!(frozen < 30, "最新的内容要留在活动区，不能全冻结");

        // 已冻结的行按顺序排在活动区上方，终端历史可读且不乱序
        let rendered = rendered_rows(&terminal);
        assert_eq!(rendered[0], "MESSAGE-00", "最旧的一行应排在最上面");
        for i in 0..frozen {
            assert_eq!(
                rendered[i],
                format!("MESSAGE-{i:02}"),
                "第 {i} 行错位: {rendered:?}"
            );
        }
        // 最新的内容仍在屏幕上可被重绘（streaming 的语义）
        let all = rendered.join("\n");
        assert!(all.contains("MESSAGE-29"), "最新的内容必须还在屏幕上");
    }

    #[test]
    fn freezing_is_monotonic_and_never_reinserts() {
        let (mut terminal, mut window) = inline_fixture(40, 20, 5);
        let mut app = new_app();
        app.welcome = false;
        for i in 0..10 {
            app.push_line(format!("A-{i:02}"));
        }
        inline_terminal(&mut window, &mut app, 40, &mut terminal);
        let first = app.frozen();
        assert!(first > 0);
        // 第一次冻结：这些行已经交给终端，但屏幕还有位置，所以"冻结"≠"已滚出屏幕"
        terminal.draw(|frame| app.draw(frame)).unwrap();
        for i in 0..first {
            assert_eq!(rendered_rows(&terminal)[i], format!("A-{i:02}"));
        }

        // 又来了 5 行：只应冻结新增的部分
        for i in 0..5 {
            app.push_line(format!("B-{i:02}"));
        }
        inline_terminal(&mut window, &mut app, 40, &mut terminal);
        assert!(app.frozen() >= first, "冻结进度只能前进");
        assert_eq!(app.frozen(), window.freeze_count(15));
        terminal.draw(|frame| app.draw(frame)).unwrap();

        // 关键不变式：所有内容行在屏幕上恰好出现一次（不重复、也不丢行）
        // 冻结前缀之上、活动区之下的每一行都必须能被看到。
        let rendered = rendered_rows(&terminal);
        for i in 0..10 {
            let needle = format!("A-{i:02}");
            let hits = rendered.iter().filter(|row| row.as_str() == needle).count();
            assert_eq!(hits, 1, "{needle} 出现了 {hits} 次: {rendered:?}");
        }
        for i in 0..5 {
            let needle = format!("B-{i:02}");
            let hits = rendered.iter().filter(|row| row.as_str() == needle).count();
            // 活动区贴底显示最新内容：最旧的一行在被挤出时可能已经被冻结覆盖，
            // 但绝不能出现"同一行在屏幕上两个地方都在"。
            assert!(hits <= 1, "{needle} 出现了 {hits} 次: {rendered:?}");
        }
        // 最新的内容必须可见（活动区贴底）
        assert!(
            rendered.join("\n").contains("B-04"),
            "最新消息必须可见: {rendered:?}"
        );

        // 冻结是单调的：内容没增长时再冻结一次不产生任何新行
        let before = app.frozen();
        inline_terminal(&mut window, &mut app, 40, &mut terminal);
        assert_eq!(app.frozen(), before);
    }

    #[test]
    fn short_history_stays_in_the_active_viewport() {
        let (mut terminal, mut window) = inline_fixture(40, 20, 5);
        let mut app = new_app();
        app.welcome = false;
        app.push_line("only-one".to_string());

        inline_terminal(&mut window, &mut app, 40, &mut terminal);
        terminal.draw(|frame| app.draw(frame)).unwrap();

        // 内容少于一屏时不该有任何东西滚出去
        assert_eq!(scrollback_rows(&terminal).len(), 0);
        assert!(screen_rows(&terminal).join("\n").contains("only-one"));
        assert_eq!(app.frozen(), 0);
    }

    #[test]
    fn freeze_on_an_empty_app_is_a_noop() {
        let (mut terminal, mut window) = inline_fixture(40, 20, 5);
        let mut app = new_app();
        app.welcome = false;
        inline_terminal(&mut window, &mut app, 40, &mut terminal);
        assert_eq!(scrollback_rows(&terminal).len(), 0);
        assert_eq!(app.frozen(), 0);
    }

    #[test]
    fn frozen_lines_are_padded_to_the_viewport_width() {
        let (mut terminal, mut window) = inline_fixture(24, 20, 5);
        let mut app = new_app();
        app.welcome = false;
        for i in 0..12 {
            app.push_line(format!("X{i}"));
        }
        inline_terminal(&mut window, &mut app, 24, &mut terminal);
        // 每一行都补位到整行宽，避免上一帧残留字符透出来
        assert!(app.frozen() > 0, "12 行内容应当有部分被冻结");
        let rendered = rendered_rows(&terminal);
        for (index, row) in rendered.iter().enumerate().take(app.frozen()) {
            assert_eq!(row, format!("X{index}").as_str(), "第 {index} 行内容错位");
        }
        // 补位宽度：冻结行铺满视口宽度（末尾都是空格）
        let raw = rendered
            .get(app.frozen().saturating_sub(1))
            .cloned()
            .unwrap_or_default();
        assert_eq!(raw, format!("X{}", app.frozen() - 1));
    }

    #[test]
    fn overflowing_screen_pushes_frozen_lines_into_scrollback() {
        // 行内视口越矮、内容越多，越早把最上面的行挤出屏幕进入终端历史
        let (mut terminal, mut window) = inline_fixture(30, 10, 4);
        let mut app = new_app();
        app.welcome = false;
        for i in 0..20 {
            app.push_line(format!("L{i:02}"));
        }
        // 每行插入后都重绘一次，模拟真实的逐行冻结节奏
        for _ in 0..20 {
            inline_terminal(&mut window, &mut app, 30, &mut terminal);
            terminal.draw(|frame| app.draw(frame)).unwrap();
        }
        let sb = scrollback_rows(&terminal);
        assert!(!sb.is_empty(), "内容超出屏幕后必须滚进终端 scrollback");
        assert_eq!(sb[0], "L00", "最先滚出去的是最旧的行: {sb:?}");
        // scrollback 里保持顺序且不重复
        for (index, row) in sb.iter().enumerate() {
            assert_eq!(row, &format!("L{index:02}"), "第 {index} 行错位: {sb:?}");
        }
    }

    #[test]
    fn editing_moves_the_cursor_and_deletes() {
        let mut app = new_app();
        for c in "abc".chars() {
            app.on_key(press(KeyCode::Char(c)));
        }
        assert_eq!(app.input.text(), "abc");
        app.on_key(press(KeyCode::Left));
        app.on_key(press(KeyCode::Backspace));
        assert_eq!(app.input.text(), "ac");
        assert_eq!(app.input.cursor(), (0, 1), "光标应停在 a| 之后");
        app.on_key(press(KeyCode::Home));
        app.on_key(press(KeyCode::Delete));
        assert_eq!(app.input.text(), "c");
        app.on_key(press(KeyCode::End));
        app.on_key(press(KeyCode::Char('中')));
        assert_eq!(app.input.text(), "c中");
        // 多字节字符的回退不能切坏字符
        app.on_key(press(KeyCode::Backspace));
        assert_eq!(app.input.text(), "c");
    }

    #[test]
    fn enter_submits_and_exit_keys_quit() {
        let mut app = new_app();
        assert_eq!(
            app.on_key(press(KeyCode::Enter)),
            Action::None,
            "空输入不提交"
        );
        for c in "你好".chars() {
            app.on_key(press(KeyCode::Char(c)));
        }
        assert_eq!(
            app.on_key(press(KeyCode::Enter)),
            Action::Submit("你好".into())
        );
        assert!(app.input.is_empty(), "提交后输入框清空");

        let mut app = new_app();
        for c in "/exit".chars() {
            app.on_key(press(KeyCode::Char(c)));
        }
        assert_eq!(app.on_key(press(KeyCode::Enter)), Action::Quit);

        let mut app = new_app();
        assert_eq!(app.on_key(ctrl('d')), Action::Quit, "空输入 Ctrl+D 退出");
        app.on_key(press(KeyCode::Char('x')));
        assert_eq!(app.on_key(ctrl('d')), Action::None, "有输入时不退出");
    }

    #[test]
    fn ctrl_c_clears_then_needs_two_presses_to_quit() {
        let mut app = new_app();
        app.on_key(press(KeyCode::Char('x')));
        // 有输入：第一次只清空
        assert_eq!(app.on_key(ctrl('c')), Action::None);
        assert!(app.input.is_empty());
        // 无输入：第一次开始计时并提示
        assert_eq!(app.on_key(ctrl('c')), Action::None);
        assert!(app.hint.is_some(), "应当提示再按一次退出");
        // 计时内再按一次才退出
        assert_eq!(app.on_key(ctrl('c')), Action::Quit);
    }

    #[test]
    fn paste_is_inserted_without_submitting() {
        let mut app = new_app();
        assert_eq!(app.on_key(Event::Paste("一\n二".into())), Action::None);
        assert_eq!(app.input.text(), "一\n二");
        // 粘贴带换行的文本绝不能顺带提交
        assert_eq!(app.input.height(6), 2);
    }

    #[test]
    fn multiline_input_grows_and_navigates() {
        let mut app = new_app();
        app.on_key(Event::Paste("第一行\n第二行".into()));
        assert_eq!(app.input.text(), "第一行\n第二行");
        assert_eq!(app.input.height(6), 2, "两行内容要占两行");
        assert_eq!(
            app.input.cursor(),
            (1, 6),
            "粘贴后光标在最后一行末尾（列按显示宽度算：三个汉字六列）"
        );

        // ↑ 回到上一行，而不是去翻历史
        app.on_key(press(KeyCode::Up));
        assert_eq!(app.input.cursor().0, 0);
        // 已经在首行时，↑ 才归历史滚动
        assert!(!app.scroll_event(&press(KeyCode::Up)));

        // ↓ 回到末行
        app.on_key(press(KeyCode::Down));
        assert_eq!(app.input.cursor().0, 1);

        // Enter 仍然是提交，提交内容包含换行
        assert_eq!(
            app.on_key(press(KeyCode::Enter)),
            Action::Submit("第一行\n第二行".into())
        );
        assert!(app.input.is_empty());
    }

    #[test]
    fn input_height_is_capped() {
        let mut app = new_app();
        app.on_key(Event::Paste("1\n2\n3\n4\n5\n6\n7\n8".into()));
        // 8 行内容，但活动区最多给 3 行：显示会滚动，高度不失控
        assert_eq!(app.input.height(3), 3);
    }

    #[test]
    fn home_and_end_stay_inside_the_input() {
        let mut app = new_app();
        app.on_key(Event::Paste("aa\nbb".into()));
        // Ctrl+E 到末行行尾；Ctrl+A 回到首行行首
        app.on_key(ctrl('e'));
        assert_eq!(app.input.cursor(), (1, 2));
        app.on_key(ctrl('a'));
        assert_eq!(app.input.cursor(), (0, 0));
    }

    #[test]
    fn multiline_input_is_rendered_by_the_textarea() {
        let mut app = new_app();
        app.on_key(Event::Paste("上\n下".into()));
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(30, 8)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let text: String = {
            let buf = terminal.backend().buffer();
            (0..buf.area.height)
                .map(|y| {
                    (0..buf.area.width)
                        .map(|x| buf[(x, y)].symbol().to_string())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert!(text.contains('上'), "{text}");
        assert!(text.contains('下'), "{text}");
    }

    #[test]
    fn events_flow_into_the_scrollback() {
        let mut app = new_app();
        app.turn_started("看看仓库");
        let opened: Vec<String> = app.take_pending().iter().map(|l| l.to_string()).collect();
        assert!(
            opened.iter().any(|l| l.contains("› 看看仓库")),
            "{opened:?}"
        );

        // 助手正文：冻结的部分进滚动区，没冻结的留在尾部（两者合起来不丢内容）
        app.on_agent_event(AgentEvent::TextDelta("- 第一\n- 第二\n".into()));
        let mut all: Vec<String> = app.take_pending().iter().map(|l| l.to_string()).collect();
        all.extend(app.tail.iter().map(|l| l.to_string()));
        let joined = all.join("\n");
        assert!(joined.contains("第一"), "{joined}");
        assert!(joined.contains("第二"), "{joined}");

        app.on_agent_event(AgentEvent::ToolFinished {
            call_id: "c1".into(),
            ok: true,
            summary: "Bash(cargo test)".into(),
            preview: Vec::new(),
            elapsed: Duration::from_millis(1200),
        });
        app.on_agent_event(AgentEvent::TurnFinished {
            usage: None,
            steps: 2,
        });
        app.turn_finished();
        let text: Vec<String> = app.take_pending().iter().map(|l| l.to_string()).collect();
        assert!(
            text.iter().any(|l| l.contains("✓ Bash(cargo test)")),
            "{text:?}"
        );
        assert!(text.iter().any(|l| l.contains("步")), "{text:?}");
        assert!(app.tail.is_empty(), "一轮结束后尾部清空：{:?}", app.tail);
        assert!(!app.running);
    }

    /// 用 TestBackend 验证绘制：输入行、状态栏与光标位置（正文走滚动区，不在活动区）。
    #[test]
    fn draws_input_and_status_bar() {
        use ratatui::{Terminal, backend::TestBackend};

        let mut app = new_app();
        app.turn_started("你好");
        app.on_agent_event(AgentEvent::TextDelta("回答一行\n".into()));
        app.turn_finished();
        for c in "接着问".chars() {
            app.on_key(press(KeyCode::Char(c)));
        }
        let scrolled: Vec<String> = app.take_pending().iter().map(|l| l.to_string()).collect();
        assert!(
            scrolled.iter().any(|l| l.contains("› 你好")),
            "用户消息进滚动区：{scrolled:?}"
        );
        assert!(
            scrolled.iter().any(|l| l.contains("回答一行")),
            "助手回复进滚动区：{scrolled:?}"
        );

        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();

        let buffer = terminal.backend().buffer().clone();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer.cell((x, y)).map(|cell| cell.symbol()).unwrap_or(" "));
            }
            text.push('\n');
        }
        // CJK 是宽字符：TestBackend 每个字符占两格，断言前先去掉空白
        let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("›接着问"), "输入行要有当前输入：{text}");
        assert!(compact.contains("确认模式"), "状态栏要有模式：{text}");
        assert!(compact.contains("deepseek-flash"), "状态栏要有模型：{text}");
        assert!(
            !compact.contains("回答一行"),
            "已经写进滚动区的内容不该还留在活动区：{text}"
        );
        let cursor = terminal.get_cursor_position().unwrap();
        assert_eq!(cursor.y, 10, "光标应在输入行（倒数第二行）：{cursor:?}");
    }

    fn bash_view(rememberable: bool) -> AskView {
        use seanbot_core::{PermissionRequest, Risk};
        AskView {
            tool: "bash".into(),
            request: PermissionRequest {
                tool: "bash".into(),
                title: "Bash 想要执行".into(),
                risk: Risk::Mutating,
                args: serde_json::json!({"command": "cargo test"}),
            },
            rememberable,
            preview: vec!["$ cargo test".into()],
        }
    }

    /// 用 TestBackend 画一帧，返回整屏文本（测试画面用）。
    fn drawn(app: &mut App, width: u16, height: u16) -> String {
        use ratatui::{Terminal, backend::TestBackend};

        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer.cell((x, y)).map(|cell| cell.symbol()).unwrap_or(" "));
            }
            text.push('\n');
        }
        text
    }

    /// 去掉所有空白，便于对 CJK 宽字符做断言。
    fn compact(text: &str) -> String {
        text.chars().filter(|c| !c.is_whitespace()).collect()
    }

    #[test]
    fn welcome_screen_has_the_mascot_box_and_metadata() {
        let mut app = new_app();
        let screen = drawn(&mut app, 72, 12);
        let text = compact(&screen);
        assert!(text.contains("┌"), "欢迎框要有左边框：{screen}");
        assert!(text.contains("┘"), "欢迎框要有右下角：{screen}");
        assert!(text.contains("Seanbotv"), "紧凑断言里没有空格：{screen}");
        assert!(text.contains("deepseek-flash"), "{screen}");
        assert!(text.contains("确认模式"), "{screen}");
        assert!(text.contains("ctrl+o"), "{screen}");
        // 环环的半块像素画在欢迎框里
        assert!(
            text.contains('\u{2580}') || text.contains('\u{2584}'),
            "欢迎框里应当有环环：{screen}"
        );
    }

    #[test]
    fn slash_popup_screen_lists_commands_and_marks_the_selection() {
        let mut app = new_app();
        for c in "/mo".chars() {
            app.on_key(press(KeyCode::Char(c)));
        }
        let screen = drawn(&mut app, 72, 14);
        let text = compact(&screen);
        assert!(text.contains("/model"), "{screen}");
        assert!(text.contains("/mouse"), "{screen}");
        assert!(text.contains("▸/model"), "选中项要有标记：{screen}");
        assert!(text.contains("切换模型"), "要显示说明：{screen}");
    }

    #[test]
    fn confirm_screen_shows_preview_and_three_choices() {
        let mut app = new_app();
        app.open_confirm(bash_view(true), Arc::new(Mutex::new(Rules::default())));
        let screen = drawn(&mut app, 72, 16);
        let text = compact(&screen);
        assert!(text.contains("需要确认"), "要有标题：{screen}");
        assert!(text.contains("$cargotest"), "要显示命令预览：{screen}");
        assert!(text.contains("1.允许"), "{screen}");
        assert!(text.contains("2.允许，本会话不再询问"), "{screen}");
        assert!(text.contains("3.拒绝，并告诉环环原因"), "{screen}");
    }

    #[test]
    fn confirm_screen_hides_the_remember_option_outside_the_project() {
        let mut app = new_app();
        app.open_confirm(bash_view(false), Arc::new(Mutex::new(Rules::default())));
        let screen = compact(&drawn(&mut app, 72, 16));
        assert!(screen.contains("1.允许"), "{screen}");
        assert!(
            !screen.contains("本会话不再询问"),
            "不该提供记住选项：{screen}"
        );
    }

    #[test]
    fn status_bar_reflects_the_permission_mode() {
        let mut app = new_app();
        let confirm = compact(&drawn(&mut app, 72, 12));
        assert!(confirm.contains("确认模式"), "{confirm}");
        app.set_mode(seanbot_core::PermissionMode::Yolo);
        let yolo = compact(&drawn(&mut app, 72, 12));
        assert!(yolo.contains("YOLO"), "{yolo}");
        assert!(!yolo.contains("确认模式"), "{yolo}");
    }

    #[test]
    fn key_sequence_drives_the_state_machine() {
        let mut app = new_app();
        // 输入 /mo → 浮窗；↓ 选中 /mouse；Tab 补全；Esc 关窗
        for c in "/mo".chars() {
            app.on_key(press(KeyCode::Char(c)));
        }
        assert!(app.popup.is_some());
        app.on_key(press(KeyCode::Down));
        app.on_key(press(KeyCode::Tab));
        assert_eq!(app.input.text(), "/mouse");
        app.on_key(press(KeyCode::Esc));
        assert!(app.popup.is_none());

        // Ctrl+U 清空 → 输入问题 → Enter 提交
        app.on_key(ctrl('u'));
        assert!(app.input.is_empty());
        for c in "你好".chars() {
            app.on_key(press(KeyCode::Char(c)));
        }
        assert_eq!(
            app.on_key(press(KeyCode::Enter)),
            Action::Submit("你好".into())
        );
        assert!(app.popup.is_none());

        // Ctrl+O 开转录
        assert_eq!(app.on_key(ctrl('o')), Action::OpenTranscript);
        // 空闲态 Ctrl+C 两次退出
        assert_eq!(app.on_key(ctrl('c')), Action::None);
        assert_eq!(app.on_key(ctrl('c')), Action::Quit);
    }

    #[test]
    fn welcome_box_shows_first_then_freezes_into_the_history() {
        let mut app = new_app();
        // 还没发消息：历史区顶部显示欢迎框
        let lines = app.history_lines();
        let text: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        let joined = text.join("\n");
        assert!(joined.contains("Seanbot v"), "欢迎框应当显示：{joined}");
        assert!(joined.contains("deepseek-flash"), "{joined}");
        assert!(app.take_pending().is_empty(), "还没发消息时不该有滚动内容");

        // 发出第一条消息：欢迎框定格进历史，首屏不再单独显示它
        app.turn_started("你好");
        let frozen: Vec<String> = app.take_pending().iter().map(|l| l.to_string()).collect();
        let frozen_text = frozen.join("\n");
        assert!(
            frozen_text.contains("Seanbot v"),
            "欢迎框应当写进滚动区：{frozen_text}"
        );
        assert!(frozen_text.contains("› 你好"), "{frozen_text}");
        let after: Vec<String> = app.history_lines().iter().map(|l| l.to_string()).collect();
        assert!(
            !after.join("\n").contains("Seanbot v"),
            "发出消息后活动区不该再显示欢迎框：{after:?}"
        );
    }

    #[test]
    fn mascot_mood_follows_the_turn() {
        let mut app = new_app();
        assert_eq!(app.mascot.mood, mascot::Mood::Idle, "刚进来是待机");
        app.turn_started("你好");
        assert_eq!(
            app.mascot.mood,
            mascot::Mood::Thinking,
            "发消息时进入思考态"
        );
        app.on_agent_event(AgentEvent::TurnFinished {
            usage: None,
            steps: 1,
        });
        assert_eq!(app.mascot.mood, mascot::Mood::Happy);
        app.turn_started("再来");
        app.on_agent_event(AgentEvent::Cancelled);
        assert_eq!(app.mascot.mood, mascot::Mood::Error);
        app.on_agent_event(AgentEvent::Error("崩了".into()));
        assert_eq!(app.mascot.mood, mascot::Mood::Error);
    }

    #[test]
    fn scroll_keys_move_the_history_window() {
        let mut app = new_app();
        app.turn_started("你好");
        for i in 0..30 {
            app.push_line(format!("第 {i} 行"));
        }
        assert_eq!(app.scroll, 0, "默认贴底");
        assert!(app.scroll_event(&press(KeyCode::PageUp)));
        assert_eq!(app.scroll, 10);
        assert!(app.scroll_event(&press(KeyCode::PageDown)));
        assert_eq!(app.scroll, 0);
        assert!(app.scroll_event(&press(KeyCode::Home)));
        assert_eq!(app.scroll, u16::MAX, "Home 拉到最早");
        assert!(app.scroll_event(&press(KeyCode::End)));
        assert_eq!(app.scroll, 0, "End 回到最新");
        assert!(
            !app.scroll_event(&press(KeyCode::Char('a'))),
            "普通按键不消费"
        );
        assert!(
            app.history_lines().len() >= 30,
            "历史里有输出：{}",
            app.history_lines().len()
        );
    }

    #[test]
    fn running_tool_uses_the_pre_tui_spinner() {
        let mut app = new_app();
        app.on_agent_event(AgentEvent::ToolStarted {
            call_id: "c1".into(),
            name: "bash".into(),
            // 事件里的 title 是原始参数，标签由 format::tool_label 统一拼
            title: "cargo test".into(),
        });
        // 状态行显示「⠋ 标签 用时」，和 TUI 之前那套一致
        let status = app.status_line().to_string();
        assert!(status.contains("Bash(cargo test)"), "{status}");
        assert!(
            crate::render::FRAMES
                .iter()
                .any(|frame| status.contains(frame)),
            "应当用行内渲染那套盲文转圈：{status}"
        );
        // 运行中的工具不占滚动区，也不该出现 emoji
        let pushed: Vec<String> = app.take_pending().iter().map(|l| l.to_string()).collect();
        assert!(pushed.is_empty(), "运行中的工具不进滚动区：{pushed:?}");

        app.on_agent_event(AgentEvent::ToolFinished {
            call_id: "c1".into(),
            ok: true,
            summary: "退出码 0".into(),
            preview: Vec::new(),
            elapsed: Duration::from_millis(1200),
        });
        let done: Vec<String> = app.take_pending().iter().map(|l| l.to_string()).collect();
        assert_eq!(done.len(), 1, "{done:?}");
        assert!(done[0].starts_with("✓ Bash(cargo test) · "), "{done:?}");
        assert!(!done[0].contains('⏳'), "{done:?}");
    }

    #[test]
    fn ctrl_o_opens_the_transcript_view() {
        let mut app = new_app();
        assert_eq!(app.on_key(ctrl('o')), Action::OpenTranscript);
    }

    #[tokio::test]
    async fn transcript_records_full_tool_results() {
        use seanbot_provider::{Message, Role};

        let mut app = new_app();
        app.turn_started("看看仓库");
        app.on_agent_event(AgentEvent::ToolStarted {
            call_id: "c1".into(),
            name: "bash".into(),
            // 事件里的 title 是原始参数，标签由 format::tool_label 统一拼
            title: "cargo test".into(),
        });
        app.on_agent_event(AgentEvent::ToolFinished {
            call_id: "c1".into(),
            ok: true,
            summary: "测试通过".into(),
            preview: Vec::new(),
            elapsed: Duration::from_millis(1200),
        });
        app.on_agent_event(AgentEvent::MessageAppended(Message {
            role: Role::Tool,
            content: "running 3 tests\nall ok".into(),
            reasoning: None,
            tool_calls: Vec::new(),
            tool_call_id: Some("c1".into()),
        }));
        app.on_agent_event(AgentEvent::MessageAppended(Message {
            role: Role::Assistant,
            content: "都过了".into(),
            reasoning: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
        }));

        let entries = app.transcript();
        assert_eq!(entries.len(), 3, "用户 + 工具 + 助手：{entries:?}");
        assert!(matches!(&entries[0], Entry::User(text) if text == "看看仓库"));
        match &entries[1] {
            Entry::Tool {
                name,
                ok,
                summary,
                content,
                ..
            } => {
                assert_eq!(name, "bash");
                assert!(*ok);
                assert_eq!(summary, "测试通过");
                assert!(content.contains("all ok"), "工具结果要完整：{content}");
            }
            other => panic!("应当是工具条目：{other:?}"),
        }
        assert!(matches!(&entries[2], Entry::Assistant(text) if text == "都过了"));
    }

    #[test]
    fn transcript_drops_a_retracted_user_message() {
        let mut app = new_app();
        app.turn_started("被取消的问题");
        assert_eq!(app.transcript().len(), 1);
        app.on_agent_event(AgentEvent::MessageRetracted);
        assert!(app.transcript().is_empty(), "撤回后不该留下这条消息");
    }

    #[tokio::test]
    async fn confirm_enter_allows_and_esc_denies() {
        let (mut app, mut events) = app_with_events();
        let rules = Arc::new(Mutex::new(Rules::default()));

        app.open_confirm(bash_view(true), rules.clone());
        assert!(app.confirm.is_some());
        app.on_key(press(KeyCode::Enter));
        assert_eq!(
            events.try_recv().unwrap(),
            UserEvent::PermissionDecision {
                tool: "bash".into(),
                allow: true,
                reason: None,
            },
            "允许：界面只发事件，回传给内核由运行时负责"
        );
        assert!(app.confirm.is_none(), "答复之后关掉确认框");

        app.open_confirm(bash_view(true), rules);
        app.on_key(press(KeyCode::Esc));
        assert_eq!(
            events.try_recv().unwrap(),
            UserEvent::PermissionDecision {
                tool: "bash".into(),
                allow: false,
                reason: None,
            }
        );
    }

    #[tokio::test]
    async fn confirm_second_option_remembers_the_rule() {
        let (mut app, mut events) = app_with_events();
        let rules = Arc::new(Mutex::new(Rules::default()));
        app.open_confirm(bash_view(true), rules.clone());
        // 第二项：允许，本会话不再询问
        app.on_key(press(KeyCode::Down));
        app.on_key(press(KeyCode::Enter));
        assert_eq!(
            events.try_recv().unwrap(),
            UserEvent::PermissionDecision {
                tool: "bash".into(),
                allow: true,
                reason: None,
            }
        );

        let remembered = {
            let rules = rules.lock().unwrap();
            rules.allows(&seanbot_core::PermissionRequest {
                tool: "bash".into(),
                title: String::new(),
                risk: seanbot_core::Risk::Mutating,
                args: serde_json::json!({"command": "cargo test -p seanbot-core"}),
            })
        };
        assert!(remembered, "应当记住 cargo test 这个前缀");
    }

    #[tokio::test]
    async fn confirm_reason_flows_back_to_the_runtime() {
        let (mut app, mut events) = app_with_events();
        app.open_confirm(bash_view(true), Arc::new(Mutex::new(Rules::default())));
        // 第三项：拒绝并说原因
        app.on_key(press(KeyCode::Down));
        app.on_key(press(KeyCode::Down));
        app.on_key(press(KeyCode::Enter));
        assert!(
            app.confirm.as_ref().unwrap().reason.is_some(),
            "应当展开原因输入"
        );
        for c in "别动生产".chars() {
            app.on_key(press(KeyCode::Char(c)));
        }
        app.on_key(press(KeyCode::Enter));
        assert_eq!(
            events.try_recv().unwrap(),
            UserEvent::PermissionDecision {
                tool: "bash".into(),
                allow: false,
                reason: Some("别动生产".into()),
            }
        );
    }

    #[test]
    fn confirm_without_the_remember_option_has_two_choices() {
        let mut app = new_app();
        app.open_confirm(bash_view(false), Arc::new(Mutex::new(Rules::default())));
        let options = app.confirm.as_ref().unwrap().options().to_vec();
        assert_eq!(options.len(), 2, "{options:?}");
        assert!(options[1].contains("拒绝"), "{options:?}");
    }

    #[tokio::test]
    async fn ending_the_turn_closes_a_pending_confirmation() {
        let (mut app, mut events) = app_with_events();
        app.open_confirm(bash_view(true), Arc::new(Mutex::new(Rules::default())));
        app.on_agent_event(AgentEvent::Cancelled);
        app.turn_finished();
        assert!(app.confirm.is_none(), "本轮结束应当关掉确认框");
        // 界面对这次询问什么都没答：内核那边的拒绝由运行时丢掉待答复名单来完成
        assert!(events.try_recv().is_err(), "收尾不该凭空造一条授权答复");
    }

    /// 提交与取消都只发事件（界面不直接调内核）。
    #[test]
    fn submit_and_cancel_go_through_the_event_channel() {
        let (mut app, mut events) = app_with_events();
        for c in "你好".chars() {
            app.on_key(press(KeyCode::Char(c)));
        }
        let action = app.on_key(press(KeyCode::Enter));
        assert_eq!(action, Action::Submit("你好".into()), "按键仍然返回动作");
        app.submit("你好".into());
        assert_eq!(events.try_recv().unwrap(), UserEvent::Submit("你好".into()));

        app.cancel();
        assert_eq!(events.try_recv().unwrap(), UserEvent::Cancel);
        assert!(events.try_recv().is_err(), "不该有多余事件");
    }

    #[test]
    fn picker_moves_and_chooses() {
        let mut app = new_app();
        app.open_picker(
            PickKind::Model,
            "切换模型",
            vec![
                PickerItem {
                    label: "  a".into(),
                    detail: "d".into(),
                    value: "a".into(),
                },
                PickerItem {
                    label: "  b".into(),
                    detail: "d".into(),
                    value: "b".into(),
                },
            ],
        );
        assert!(app.picker.is_some());
        assert_eq!(app.on_key(press(KeyCode::Down)), Action::None);
        assert_eq!(
            app.on_key(press(KeyCode::Enter)),
            Action::Choose(PickKind::Model, "b".into())
        );
        assert!(app.picker.is_none(), "选完就关掉列表");
    }

    #[test]
    fn picker_closes_on_escape_and_reports_empty_lists() {
        let mut app = new_app();
        app.open_picker(PickKind::Session, "恢复会话", Vec::new());
        assert!(app.picker.is_none(), "没有可选项时不打开列表");
        let lines: Vec<String> = app.take_pending().iter().map(|l| l.to_string()).collect();
        assert!(lines.iter().any(|l| l.contains("没有可选项")), "{lines:?}");

        app.open_picker(
            PickKind::Session,
            "恢复会话",
            vec![PickerItem {
                label: "会话".into(),
                detail: "1 条消息".into(),
                value: "/tmp/x.jsonl".into(),
            }],
        );
        assert_eq!(app.on_key(press(KeyCode::Esc)), Action::None);
        assert!(app.picker.is_none(), "Esc 关掉列表");
    }

    #[test]
    fn typing_a_slash_opens_and_filters_the_popup() {
        let mut app = new_app();
        for c in "/mo".chars() {
            app.on_key(press(KeyCode::Char(c)));
        }
        let popup = app.popup.as_ref().expect("应当弹出浮窗");
        let names: Vec<&str> = popup
            .matches
            .iter()
            .map(|m| slash::COMMANDS[m.index].name)
            .collect();
        assert_eq!(names, vec!["/model", "/mouse"]);
        assert_eq!(popup.selected, 0);

        app.on_key(press(KeyCode::Down));
        assert_eq!(app.popup.as_ref().unwrap().selected, 1);
        app.on_key(press(KeyCode::Tab));
        assert_eq!(app.input.text(), "/mouse", "Tab 应当补全选中的命令");
        assert!(app.popup.is_some(), "补全后仍是命令名，浮窗留着");

        app.on_key(press(KeyCode::Esc));
        assert!(app.popup.is_none(), "Esc 关掉浮窗");
        // 关掉后方向键恢复成光标移动
        app.on_key(press(KeyCode::Left));
        assert_eq!(app.input.text(), "/mouse");
    }

    #[test]
    fn enter_runs_the_selected_command_and_space_closes_it() {
        let mut app = new_app();
        for c in "/he".chars() {
            app.on_key(press(KeyCode::Char(c)));
        }
        assert_eq!(app.on_key(press(KeyCode::Enter)), Action::Command("/help"));
        assert!(app.input.is_empty(), "执行后清空输入");
        assert!(app.popup.is_none());

        let mut app = new_app();
        for c in "/model ".chars() {
            app.on_key(press(KeyCode::Char(c)));
        }
        assert!(app.popup.is_none(), "有空格就不再当成命令名");
    }

    #[test]
    fn popup_is_drawn_above_the_input() {
        use ratatui::{Terminal, backend::TestBackend};

        let mut app = new_app();
        app.on_key(press(KeyCode::Char('/')));
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();

        let buffer = terminal.backend().buffer().clone();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer.cell((x, y)).map(|cell| cell.symbol()).unwrap_or(" "));
            }
            text.push('\n');
        }
        assert!(text.contains("/help"), "浮窗要列出命令：{text}");
        assert!(text.contains("/exit"), "{text}");
        assert!(text.contains("▸ /help"), "选中项要有标记：{text}");
    }

    #[test]
    fn long_history_stays_inside_the_app() {
        // 全屏模式：所有输出都留在应用内，不会再被写进终端滚动区
        let mut app = new_app();
        app.turn_started("你好");
        for i in 0..500 {
            app.push_line(format!("第 {i} 行"));
        }
        assert!(app.history_lines().len() >= 500);
        assert!(app.take_pending().len() >= 500);
    }
}
