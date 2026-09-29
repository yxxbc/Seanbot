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

mod mascot;
mod permission;
mod slash;
mod transcript;

pub use permission::{Ask, Rules, TuiPermission};
pub use transcript::Entry;

use std::{
    collections::HashMap,
    io::{self, Write},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::Context;
use crossterm::{
    event::{
        DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal, TerminalOptions, Viewport,
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};
use seanbot_core::{Agent, AgentEvent, Decision, config::Config};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use unicode_width::UnicodeWidthStr;

use crate::{format, journal::Journal, render, repl};

/// 活动区高度（阶段 2c 会改成随内容动态变化）。
const MIN_VIEWPORT_HEIGHT: u16 = 4;
/// 动画/重绘节拍。
const TICK: Duration = Duration::from_millis(80);
/// 连按两次 Ctrl+C 退出的时间窗。
const QUIT_WINDOW: Duration = Duration::from_secs(2);
/// 提示符宽度（`› ` 占两列）。
const PROMPT_WIDTH: u16 = 2;

/// 终端守卫：进入 raw mode 与 bracketed paste，退出（含 panic）时还原。
struct Guard;

impl Guard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnableBracketedPaste)?;
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // 先把终端还原，再把 panic 交给原来的钩子（保证错误信息还能看到）
            let _ = restore_terminal();
            previous(info);
        }));
        Ok(Self)
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = restore_terminal();
    }
}

/// 还原终端：关闭 bracketed paste、显示光标、退出 raw mode。
fn restore_terminal() -> io::Result<()> {
    let mut out = io::stdout();
    // 鼠标捕获也要还回去，否则退出后终端还在捕获鼠标（滚动/选择都变怪）
    let _ = execute!(out, crossterm::event::DisableMouseCapture);
    let _ = execute!(out, DisableBracketedPaste);
    let _ = execute!(out, crossterm::cursor::Show);
    let _ = disable_raw_mode();
    let _ = out.flush();
    Ok(())
}

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

/// 单行输入框（阶段 2d 会换成多行 tui-textarea）。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Input {
    text: String,
    /// 光标位置（字符下标，0..=字符数）
    cursor: usize,
}

impl Input {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    /// 取走内容并把输入框清空（提交时用）。
    pub fn take(&mut self) -> String {
        let text = std::mem::take(&mut self.text);
        self.cursor = 0;
        text
    }

    fn insert(&mut self, c: char) {
        let at = self.byte_index();
        self.text.insert(at, c);
        self.cursor += 1;
    }

    /// 粘贴进来的整段文本（bracketed paste 不触发提交）。
    fn insert_str(&mut self, s: &str) {
        let at = self.byte_index();
        self.text.insert_str(at, s);
        self.cursor += s.chars().count();
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let end = self.byte_index();
        self.cursor -= 1;
        let start = self.byte_index();
        self.text.replace_range(start..end, "");
    }

    fn delete(&mut self) {
        if self.cursor >= self.text.chars().count() {
            return;
        }
        let start = self.byte_index();
        let mut chars = self.text.chars();
        let next = chars.nth(self.cursor);
        if let Some(c) = next {
            let end = start + c.len_utf8();
            self.text.replace_range(start..end, "");
        }
    }

    fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.text.chars().count());
    }

    fn home(&mut self) {
        self.cursor = 0;
    }

    fn end(&mut self) {
        self.cursor = self.text.chars().count();
    }

    /// 光标处的字节下标。
    fn byte_index(&self) -> usize {
        self.text
            .char_indices()
            .nth(self.cursor)
            .map(|(index, _)| index)
            .unwrap_or(self.text.len())
    }
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

/// 确认框状态（设计书 §4.8）。
struct ConfirmState {
    /// 待回传的请求；拿走它就等于关闭确认框
    ask: Option<Ask>,
    selected: usize,
    /// 正在输入拒绝原因（None 表示还在选选项）
    reason: Option<Input>,
    rules: Arc<Mutex<Rules>>,
}

impl ConfirmState {
    /// 可用选项；改动工作目录之外的 edit 不提供"本会话不再询问"。
    fn options(&self) -> Vec<&'static str> {
        let mut options = vec!["允许"];
        if self.ask.as_ref().is_some_and(|ask| ask.rememberable) {
            options.push("允许，本会话不再询问");
        }
        options.push("拒绝，并告诉环环原因");
        options
    }

    /// 确认框需要几行：预览 + 空行 + 选项，再加上下边框。
    fn height(&self) -> u16 {
        let preview = self.ask.as_ref().map(|ask| ask.preview.len()).unwrap_or(0) as u16;
        preview + self.options().len() as u16 + 4
    }
}

/// 二级列表选出来的东西要干什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickKind {
    Model,
    Session,
}

/// 二级列表里的一个候选项。
#[derive(Debug, Clone, PartialEq, Eq)]
struct PickerItem {
    label: String,
    detail: String,
    /// 选中后交给调用方的值（模型 id 或会话文件路径）
    value: String,
}

/// 二级列表状态（/model、/resume）。
#[derive(Debug)]
struct Picker {
    title: String,
    kind: PickKind,
    items: Vec<PickerItem>,
    selected: usize,
}

/// 斜杠命令浮窗状态。
#[derive(Debug, Default)]
struct Popup {
    matches: Vec<slash::Match>,
    selected: usize,
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
    pending: Vec<Line<'static>>,
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
    frame: usize,
    running: bool,
}

impl App {
    pub fn new(model: &str, cwd: &str, mode: seanbot_core::PermissionMode) -> Self {
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
            markdown: None,
            tail: Vec::new(),
            width: 80,
            left,
            mode: mode_label(mode),
            hint: None,
            quit_deadline: None,
            frame: 0,
            running: false,
        }
    }

    pub fn set_hint(&mut self, hint: String) {
        self.hint = Some(hint);
    }

    /// 处理按键。
    pub fn on_key(&mut self, event: Event) -> Action {
        match event {
            Event::Key(key) => self.on_key_press(key),
            Event::Paste(text) => {
                self.input.insert_str(&text);
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
                    self.input.home();
                    return Action::None;
                }
                KeyCode::Char('e') => {
                    self.input.end();
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
        match key.code {
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
            KeyCode::Char(c) => {
                self.input.insert(c);
                self.refresh_popup();
                Action::None
            }
            KeyCode::Backspace => {
                self.input.backspace();
                self.refresh_popup();
                Action::None
            }
            KeyCode::Delete => {
                self.input.delete();
                self.refresh_popup();
                Action::None
            }
            KeyCode::Left => {
                self.input.left();
                Action::None
            }
            KeyCode::Right => {
                self.input.right();
                Action::None
            }
            KeyCode::Home => {
                self.input.home();
                Action::None
            }
            KeyCode::End => {
                self.input.end();
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
        self.push_line(format!("› {prompt}"));
        self.transcript.push(Entry::User(prompt.to_string()));
        // 第一条消息发出：欢迎框以当前帧写进滚动区并定格
        if self.welcome {
            self.welcome = false;
            let banner = self.banner();
            self.pending.extend(banner);
        }
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
    pub fn take_pending(&mut self) -> Vec<Line<'static>> {
        std::mem::take(&mut self.pending)
    }

    /// 终端尺寸变化时更新渲染宽度。
    pub fn set_width(&mut self, width: u16) {
        self.width = width;
    }

    /// 活动区高度：流式尾部 + 输入行 + 状态栏，不超过终端高度一半。
    pub fn desired_height(&self, term_height: u16) -> u16 {
        // 没有流式内容时活动区只留 1 行：以前 max(3) 硬撑会在输入框上方留一大片空白
        let body = if self.welcome {
            8
        } else {
            (self.tail.len() as u16).max(1)
        };
        // 浮窗也要占地方，不然会把它挤掉
        let popup = self
            .popup
            .as_ref()
            .map(|popup| popup.matches.len() as u16)
            .unwrap_or(0);
        let cap = (term_height / 2).max(4);
        let picker = self
            .picker
            .as_ref()
            .map(|picker| picker.items.len() as u16 + 1)
            .unwrap_or(0);
        let confirm = self.confirm.as_ref().map(|c| c.height()).unwrap_or(0);
        (body.max(popup).max(picker).max(confirm) + 2)
            .clamp(4, cap)
            .min(term_height.max(4))
    }

    pub fn tick(&mut self) {
        self.frame = self.frame.wrapping_add(1);
        self.mascot.tick();
    }

    /// 活动区内容：还没冻结的流式尾部（只保留最后几行）。
    fn activity_lines(&self, height: usize) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();
        if self.welcome {
            lines.extend(self.banner());
        }
        lines.extend(self.tail.iter().cloned());
        let skip = lines.len().saturating_sub(height);
        lines.into_iter().skip(skip).collect()
    }

    /// 欢迎框（沿用当前帧，冻结进滚动区时也是这一帧）。
    fn banner(&self) -> Vec<Line<'static>> {
        mascot::welcome_lines(
            &self.mascot,
            env!("CARGO_PKG_VERSION"),
            &self.model_name(),
            &self.mode,
            &shorten_home(&self.cwd),
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
    pub fn open_confirm(&mut self, ask: Ask, rules: Arc<Mutex<Rules>>) {
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
                    self.send_decision(Decision::Deny { reason });
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
                        match key.code {
                            KeyCode::Char(c) => reason.insert(c),
                            KeyCode::Backspace => reason.backspace(),
                            KeyCode::Delete => reason.delete(),
                            KeyCode::Left => reason.left(),
                            KeyCode::Right => reason.right(),
                            _ => {}
                        }
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
            KeyCode::Esc => self.send_decision(Decision::Deny { reason: None }),
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
        if selected == 0 {
            self.send_decision(Decision::AllowOnce);
            return;
        }
        // 第二项：记住规则 + 放行
        if let Some(confirm) = self.confirm.as_ref()
            && let Some(ask) = confirm.ask.as_ref()
            && let Ok(mut rules) = confirm.rules.lock()
        {
            rules.remember(&ask.request);
        }
        self.send_decision(Decision::AllowSession);
    }

    /// 把决定回传给等待中的内核（顺带关闭确认框）。
    fn send_decision(&mut self, decision: Decision) {
        let Some(mut confirm) = self.confirm.take() else {
            return;
        };
        if let Some(ask) = confirm.ask.take() {
            let _ = ask.reply.send(decision);
        }
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
        let Some(query) = slash::popup_query(self.input.text()) else {
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

    fn input_line(&self) -> Line<'static> {
        Line::from(vec![
            Span::styled("› ", Style::default().fg(Color::Rgb(0xE6, 0xB8, 0x5C))),
            Span::raw(self.input.text().to_string()),
        ])
    }

    fn status_line(&self) -> Line<'static> {
        // 与 TUI 之前完全一致：跑工具时是「⠋ 标签 用时」，空闲/思考时才是环环的迷你转圈
        let left = match &self.running_tool {
            Some((label, since)) => format!(
                "{} {label} {}",
                render::FRAMES[self.frame % render::FRAMES.len()],
                format::secs(since.elapsed())
            ),
            None if self.running => "思考中…".to_string(),
            None => String::new(),
        };
        let hint = self.hint.clone().unwrap_or_default();
        let mut spans: Vec<Span<'static>> = Vec::new();
        if self.running_tool.is_none() {
            spans.push(self.mascot.mini());
        }
        spans.push(Span::styled(
            left,
            Style::default().fg(Color::Rgb(0xE6, 0xB8, 0x5C)),
        ));
        spans.push(Span::raw(format!("{} · {hint}", self.left)));
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            self.mode.clone(),
            Style::default().fg(Color::Rgb(0xD9, 0x5F, 0x4B)),
        ));
        Line::from(spans)
    }

    fn draw(&self, frame: &mut Frame) {
        let area = frame.area();
        let rows = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

        frame.render_widget(
            Paragraph::new(self.activity_lines(rows[0].height as usize)),
            rows[0],
        );
        frame.render_widget(Paragraph::new(self.input_line()), rows[1]);
        frame.render_widget(Paragraph::new(self.status_line()), rows[2]);

        // 确认框：金色边框，盖在活动区底部
        if let Some(confirm) = &self.confirm {
            let height = confirm.height().min(rows[0].height);
            if height >= 3 {
                let area = Rect {
                    x: rows[0].x,
                    y: rows[0].bottom().saturating_sub(height),
                    width: rows[0].width,
                    height,
                };
                let mut lines: Vec<Line<'static>> = confirm
                    .ask
                    .as_ref()
                    .map(|ask| ask.preview.clone())
                    .unwrap_or_default()
                    .into_iter()
                    .map(|text| Line::from(Span::raw(text)))
                    .collect();
                lines.push(Line::from(""));
                match &confirm.reason {
                    Some(reason) => lines.push(Line::from(vec![
                        Span::raw("拒绝原因："),
                        Span::styled(
                            reason.text().to_string(),
                            Style::default().fg(Color::Rgb(0xF4, 0xE9, 0xD8)),
                        ),
                    ])),
                    None => {
                        for (index, option) in confirm.options().iter().enumerate() {
                            let selected = index == confirm.selected;
                            let marker = if selected { "▸ " } else { "  " };
                            let style = if selected {
                                Style::default()
                                    .fg(Color::Rgb(0xE6, 0xB8, 0x5C))
                                    .add_modifier(Modifier::BOLD)
                            } else {
                                Style::default()
                            };
                            lines.push(Line::from(Span::styled(
                                format!("{marker}{}. {option}", index + 1),
                                style,
                            )));
                        }
                    }
                }
                let gold = Style::default().fg(Color::Rgb(0xE6, 0xB8, 0x5C));
                let block = Block::default()
                    .borders(Borders::ALL)
                    .border_style(gold)
                    .title(" 需要确认 ");
                let inner = block.inner(area);
                frame.render_widget(Clear, area);
                frame.render_widget(block, area);
                frame.render_widget(Paragraph::new(lines), inner);
            }
        }

        // 二级列表同样贴在输入行上方，第一行是标题
        if let Some(picker) = &self.picker {
            let height = (picker.items.len() as u16 + 1).min(rows[0].height);
            if height > 1 {
                let area = Rect {
                    x: rows[0].x,
                    y: rows[0].bottom().saturating_sub(height),
                    width: rows[0].width,
                    height,
                };
                let mut lines: Vec<Line<'static>> = vec![Line::from(Span::styled(
                    format!(" {} （↑↓ 选择 · Enter 确认 · Esc 取消）", picker.title),
                    Style::default().fg(Color::Rgb(0xE6, 0xB8, 0x5C)),
                ))];
                for (index, item) in picker.items.iter().enumerate() {
                    lines.push(picker_line(item, index == picker.selected));
                }
                frame.render_widget(Clear, area);
                frame.render_widget(Paragraph::new(lines), area);
            }
        }

        // 浮窗贴在输入行上方
        if let Some(popup) = &self.popup {
            let height = (popup.matches.len() as u16).min(rows[0].height);
            if height > 0 {
                let area = Rect {
                    x: rows[0].x,
                    y: rows[0].bottom().saturating_sub(height),
                    width: rows[0].width,
                    height,
                };
                let lines: Vec<Line<'static>> = popup
                    .matches
                    .iter()
                    .enumerate()
                    .map(|(index, item)| popup_line(item, index == popup.selected))
                    .collect();
                frame.render_widget(Clear, area);
                frame.render_widget(Paragraph::new(lines), area);
            }
        }

        let width = UnicodeWidthStr::width(self.input.text()) as u16;
        let x = rows[1].x + PROMPT_WIDTH + width;
        frame.set_cursor_position((x.min(rows[1].right().saturating_sub(1)), rows[1].y));
    }
}

/// 状态栏右侧的模式文案。
fn mode_label(mode: seanbot_core::PermissionMode) -> String {
    match mode {
        seanbot_core::PermissionMode::Yolo => "⚡ YOLO".to_string(),
        seanbot_core::PermissionMode::Confirm => "确认模式".to_string(),
    }
}

/// 二级列表里的一行：选中标记 + 标签 + 说明。
fn picker_line(item: &PickerItem, selected: bool) -> Line<'static> {
    let marker = if selected { "▸ " } else { "  " };
    Line::from(vec![
        Span::styled(marker, Style::default().fg(Color::Rgb(0xE6, 0xB8, 0x5C))),
        Span::raw(item.label.clone()),
        Span::styled(
            format!("  {}", item.detail),
            Style::default().fg(Color::DarkGray),
        ),
    ])
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

/// 浮窗里的一行：命令名（命中的字符高亮）+ 说明。
fn popup_line(item: &slash::Match, selected: bool) -> Line<'static> {
    let command = slash::COMMANDS[item.index];
    let marker = if selected { "▸ " } else { "  " };
    let mut spans = vec![Span::styled(
        marker,
        Style::default().fg(Color::Rgb(0xE6, 0xB8, 0x5C)),
    )];
    for (text, hit) in slash::highlight(command.name, &item.positions) {
        let style = if hit {
            Style::default()
                .fg(Color::Rgb(0xE6, 0xB8, 0x5C))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        spans.push(Span::styled(text, style));
    }
    spans.push(Span::raw(format!("  {}", command.description)));
    Line::from(spans)
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

/// 把已经冻结的行写进终端滚动区：之后由终端自己滚动，不再占活动区。
fn flush_scrollback(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> io::Result<()> {
    let pending = app.take_pending();
    if pending.is_empty() {
        return Ok(());
    }
    let height = pending.len() as u16;
    terminal.insert_before(height, |buffer| {
        Paragraph::new(pending).render(buffer.area, buffer);
    })
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
    let mut app = App::new(agent.model(), &cwd, mode);
    let _ = cfg;

    let _guard = Guard::enter().context("进入终端原始模式失败")?;
    // 配置里开了鼠标就接上（退出时由 Guard 还原）
    if cfg.ui.mouse {
        let _ = execute!(io::stdout(), crossterm::event::EnableMouseCapture);
    }
    let mut terminal = Terminal::with_options(
        CrosstermBackend::new(io::stdout()),
        TerminalOptions {
            viewport: Viewport::Inline(MIN_VIEWPORT_HEIGHT),
        },
    )
    .context("初始化行内 TUI 失败")?;
    let mut viewport_height = MIN_VIEWPORT_HEIGHT;
    let mut ticker = tokio::time::interval(TICK);

    loop {
        // 每次重绘前：更新宽度、按内容调整活动区高度、把冻结内容写进滚动区
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        app.set_width(cols);
        let desired = app.desired_height(rows);
        if desired != viewport_height {
            viewport_height = desired;
            terminal = Terminal::with_options(
                CrosstermBackend::new(io::stdout()),
                TerminalOptions {
                    viewport: Viewport::Inline(desired),
                },
            )?;
        }
        flush_scrollback(&mut terminal, &mut app)?;
        terminal.draw(|frame| app.draw(frame))?;
        let prompt = loop {
            // 按键在本线程处理（与 CPR 查询同一个线程，避免两个读取者）
            let mut decided: Option<Option<String>> = None;
            while let Some(event) = poll_event() {
                match app.on_key(event) {
                    Action::Submit(text) => {
                        decided = Some(Some(text));
                        break;
                    }
                    Action::Quit => {
                        decided = Some(None);
                        break;
                    }
                    Action::Command(name) => {
                        run_command(name, agent, journal, &cwd, &mut app, cfg).await
                    }
                    Action::OpenTranscript => {
                        transcript::show(&mut terminal, &mut app, cfg.ui.mouse).await?;
                    }
                    Action::Choose(kind, value) => {
                        apply_choice(kind, value, agent, journal, &mut app)
                    }
                    _ => {}
                }
            }
            if let Some(decided) = decided {
                break decided;
            }
            tokio::select! {
                Some(hint) = hint_rx.recv() => app.set_hint(hint),
                _ = ticker.tick() => app.tick(),
            }
            flush_scrollback(&mut terminal, &mut app)?;
            terminal.draw(|frame| app.draw(frame))?;
        };
        let Some(prompt) = prompt else {
            terminal.show_cursor()?;
            return Ok(());
        };

        // 跑一轮：界面继续响应按键，Esc / Ctrl+C 取消本轮
        let (tx, mut rx) = mpsc::channel(256);
        if let Ok(mut journal) = journal.lock() {
            journal.ensure_open();
        }
        let cancel = CancellationToken::new();
        app.turn_started(&prompt);
        let mut turn = std::pin::pin!(agent.run_turn(prompt, tx, cancel.clone()));
        loop {
            while let Some(event) = poll_event() {
                match app.on_key(event) {
                    Action::Cancel | Action::Quit => {
                        if app.running {
                            cancel.cancel();
                        } else {
                            terminal.show_cursor()?;
                            return Ok(());
                        }
                    }
                    _ => {}
                }
            }
            flush_scrollback(&mut terminal, &mut app)?;
            terminal.draw(|frame| app.draw(frame))?;
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
                    // 内核在等这次授权：弹确认框（Esc/中断/本轮结束都会回传拒绝）
                    app.open_confirm(ask, rules.clone());
                }
                _ = ticker.tick() => app.tick(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    fn new_app() -> App {
        App::new(
            "deepseek-flash",
            "/tmp/proj",
            seanbot_core::PermissionMode::Confirm,
        )
    }

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn ctrl(c: char) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
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
        assert_eq!(app.input.cursor, 1);
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

    fn bash_ask(rememberable: bool) -> (Ask, tokio::sync::oneshot::Receiver<Decision>) {
        use seanbot_core::{PermissionRequest, Risk};
        let (reply, rx) = tokio::sync::oneshot::channel();
        let request = PermissionRequest {
            tool: "bash".into(),
            title: "Bash 想要执行".into(),
            risk: Risk::Mutating,
            args: serde_json::json!({"command": "cargo test"}),
        };
        (
            Ask {
                request,
                reply,
                rememberable,
                preview: vec!["$ cargo test".into()],
            },
            rx,
        )
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
        let (ask, _rx) = bash_ask(true);
        app.open_confirm(ask, Arc::new(Mutex::new(Rules::default())));
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
        let (ask, _rx) = bash_ask(false);
        app.open_confirm(ask, Arc::new(Mutex::new(Rules::default())));
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
    fn welcome_box_shows_first_then_freezes_into_the_scrollback() {
        let mut app = new_app();
        // 还没发消息：活动区显示欢迎框
        let lines = app.activity_lines(12);
        let text: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        let joined = text.join("\n");
        assert!(joined.contains("Seanbot v"), "欢迎框应当显示：{joined}");
        assert!(joined.contains("deepseek-flash"), "{joined}");
        assert!(app.take_pending().is_empty(), "还没发消息时不该有滚动内容");

        // 发出第一条消息：欢迎框定格进滚动区，活动区不再显示它
        app.turn_started("你好");
        let frozen: Vec<String> = app.take_pending().iter().map(|l| l.to_string()).collect();
        let frozen_text = frozen.join("\n");
        assert!(
            frozen_text.contains("Seanbot v"),
            "欢迎框应当写进滚动区：{frozen_text}"
        );
        assert!(frozen_text.contains("› 你好"), "{frozen_text}");
        let after: Vec<String> = app
            .activity_lines(12)
            .iter()
            .map(|l| l.to_string())
            .collect();
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
    fn welcome_box_occupies_the_activity_area_height() {
        let mut app = new_app();
        assert_eq!(app.desired_height(40), 10, "欢迎框 8 行 + 输入行 + 状态栏");
        app.turn_started("你好");
        assert_eq!(
            app.desired_height(40),
            4,
            "发消息后只留 1 行活动区（不再用 max(3) 硬撑）"
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
        let mut app = new_app();
        let rules = Arc::new(Mutex::new(Rules::default()));

        let (ask, rx) = bash_ask(true);
        app.open_confirm(ask, rules.clone());
        assert!(app.confirm.is_some());
        app.on_key(press(KeyCode::Enter));
        assert_eq!(rx.await.unwrap(), Decision::AllowOnce);
        assert!(app.confirm.is_none(), "回传决定后关掉确认框");

        let (ask, rx) = bash_ask(true);
        app.open_confirm(ask, rules);
        app.on_key(press(KeyCode::Esc));
        assert_eq!(rx.await.unwrap(), Decision::Deny { reason: None });
    }

    #[tokio::test]
    async fn confirm_second_option_remembers_the_rule() {
        let mut app = new_app();
        let rules = Arc::new(Mutex::new(Rules::default()));
        let (ask, rx) = bash_ask(true);
        app.open_confirm(ask, rules.clone());
        app.on_key(press(KeyCode::Down));
        app.on_key(press(KeyCode::Enter));
        assert_eq!(rx.await.unwrap(), Decision::AllowSession);

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
    async fn confirm_reason_flows_back_to_the_kernel() {
        let mut app = new_app();
        let (ask, rx) = bash_ask(true);
        app.open_confirm(ask, Arc::new(Mutex::new(Rules::default())));
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
            rx.await.unwrap(),
            Decision::Deny {
                reason: Some("别动生产".into())
            }
        );
    }

    #[test]
    fn confirm_without_the_remember_option_has_two_choices() {
        let mut app = new_app();
        let (ask, _rx) = bash_ask(false);
        app.open_confirm(ask, Arc::new(Mutex::new(Rules::default())));
        let options = app.confirm.as_ref().unwrap().options().to_vec();
        assert_eq!(options.len(), 2, "{options:?}");
        assert!(options[1].contains("拒绝"), "{options:?}");
    }

    #[tokio::test]
    async fn ending_the_turn_denies_a_pending_confirmation() {
        let mut app = new_app();
        let (ask, rx) = bash_ask(true);
        app.open_confirm(ask, Arc::new(Mutex::new(Rules::default())));
        app.on_agent_event(AgentEvent::Cancelled);
        app.turn_finished();
        assert!(app.confirm.is_none(), "本轮结束应当关掉确认框");
        assert!(rx.await.is_err(), "发送端被丢弃 → 内核按拒绝处理");
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
    fn desired_height_follows_the_tail_and_the_terminal() {
        let mut app = new_app();
        assert_eq!(app.desired_height(40), 10, "欢迎框 8 行 + 输入行 + 状态栏");
        app.turn_started("你好");
        assert_eq!(
            app.desired_height(40),
            4,
            "没有流式正文时只留 1 行活动区 + 输入行 + 状态栏"
        );
        assert_eq!(app.desired_height(8), 4, "小终端下不超过一半高度");
        app.on_agent_event(AgentEvent::TextDelta(
            "未闭合的代码块\n```\n一\n二\n".into(),
        ));
        let tall = app.desired_height(40);
        assert!((4..=20).contains(&tall), "高度应当在 4..=20：{tall}");
    }
}
