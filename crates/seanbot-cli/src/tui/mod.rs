//! 行内 TUI（设计书 §4）：终端守卫、事件循环、输入框与状态栏。
//!
//! 阶段 2a 只搭骨架：raw mode / bracketed paste / panic 兜底、`Viewport::Inline` 行内界面、
//! 单行输入框、状态栏，以及把一轮对话的事件画在活动区里。斜杠浮窗、确认框、Ctrl+O 转录、
//! 鼠标、环环动画分别在 2d/2e/2f 补上。
//!
//! 已知限制（2d 解决）：确认模式下 edit/bash 的确认框仍由旧的逐行渲染器绘制，会打乱 TUI 画面；
//! 需要这些工具时先加 --yolo，或设 SEANBOT_REPL=1 回到逐行 REPL。
//!
//! 伪终端在当前开发沙箱里不可用（openpty 被拒），所以界面靠 ratatui TestBackend 断言
//! （活动区/输入行/状态栏/光标），入口与按键逻辑用单元测试覆盖。

use std::{
    io::{self, Write},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::Context;
use crossterm::{
    event::{
        DisableBracketedPaste, EnableBracketedPaste, Event, EventStream, KeyCode, KeyEvent,
        KeyEventKind, KeyModifiers,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode},
};
use futures::StreamExt;
use ratatui::{
    Frame, Terminal, TerminalOptions, Viewport,
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use seanbot_core::{Agent, AgentEvent, config::Config};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use unicode_width::UnicodeWidthStr;

use crate::{format, journal::Journal};

/// 活动区高度（阶段 2c 会改成随内容动态变化）。
const VIEWPORT_HEIGHT: u16 = 12;
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
pub struct App {
    input: Input,
    /// 已经写完的活动区内容（滚动区的一部分）
    activity: Vec<Line<'static>>,
    /// 正在流式输出的助手正文
    streaming: String,
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
            activity: Vec::new(),
            streaming: String::new(),
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
                    return Action::None;
                }
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
                Action::None
            }
            KeyCode::Backspace => {
                self.input.backspace();
                Action::None
            }
            KeyCode::Delete => {
                self.input.delete();
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
                self.streaming.push_str(&text);
                self.flush_streaming();
            }
            AgentEvent::ToolStarted { title, name, .. } => {
                self.flush_streaming();
                let label = format::tool_label(&name, &title);
                self.push_line(format!("⏳ {label}"));
            }
            AgentEvent::ToolFinished {
                ok,
                summary,
                elapsed,
                ..
            } => {
                let mark = if ok { "✓" } else { "✗" };
                self.push_line(format!("{mark} {summary} · {}", format::secs(elapsed)));
            }
            AgentEvent::TurnFinished { usage, steps } => {
                self.flush_streaming();
                self.push_line(format::usage_line(usage.as_ref(), steps));
            }
            AgentEvent::Cancelled => {
                self.flush_streaming();
                self.push_line("⎿ 已中断".to_string());
            }
            AgentEvent::Error(msg) => {
                self.flush_streaming();
                self.push_line(format!("✗ 错误：{msg}"));
            }
            _ => {}
        }
    }

    /// 一轮开始/结束。
    pub fn turn_started(&mut self, prompt: &str) {
        self.flush_streaming();
        self.push_line(format!("› {prompt}"));
        self.running = true;
    }

    pub fn turn_finished(&mut self) {
        self.flush_streaming();
        self.running = false;
    }

    /// 把流式正文按换行切进活动区（末尾不完整的行留在 streaming 里）。
    fn flush_streaming(&mut self) {
        while let Some(index) = self.streaming.find('\n') {
            let line: String = self.streaming.drain(..=index).collect();
            self.activity
                .push(Line::from(line.trim_end_matches('\n').to_string()));
        }
    }

    fn push_line(&mut self, text: String) {
        self.activity.push(Line::from(text));
    }

    pub fn tick(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    /// 活动区可见的最后几行（尾部还有正在流式的正文）。
    fn visible(&self, height: usize) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();
        let pending: Vec<Line<'static>> = if self.streaming.is_empty() {
            Vec::new()
        } else {
            self.streaming
                .lines()
                .map(|line| Line::from(line.to_string()))
                .collect()
        };
        let total = self.activity.len() + pending.len();
        let skip = total.saturating_sub(height);
        for (index, line) in self.activity.iter().enumerate() {
            if index >= skip {
                lines.push(line.clone());
            }
        }
        lines.extend(
            pending
                .into_iter()
                .skip(skip.saturating_sub(self.activity.len())),
        );
        lines
    }

    fn input_line(&self) -> Line<'static> {
        Line::from(vec![
            Span::styled("› ", Style::default().fg(Color::Rgb(0xE6, 0xB8, 0x5C))),
            Span::raw(self.input.text().to_string()),
        ])
    }

    fn status_line(&self) -> Line<'static> {
        let spinner = ["◜", "◝", "◞", "◟"][(self.frame / 2) % 4];
        let left = if self.running {
            format!("{spinner} 思考中… ")
        } else {
            String::new()
        };
        let hint = self.hint.clone().unwrap_or_default();
        Line::from(vec![
            Span::styled(left, Style::default().fg(Color::Rgb(0xE6, 0xB8, 0x5C))),
            Span::raw(format!("{} · {hint}", self.left)),
            Span::raw("  "),
            Span::styled(
                self.mode.clone(),
                Style::default().fg(Color::Rgb(0xD9, 0x5F, 0x4B)),
            ),
        ])
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
            Paragraph::new(self.visible(rows[0].height as usize)),
            rows[0],
        );
        frame.render_widget(Paragraph::new(self.input_line()), rows[1]);
        frame.render_widget(Paragraph::new(self.status_line()), rows[2]);

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

/// 启动 TUI。返回码沿用 CLI 约定。
pub async fn run(
    agent: &mut Agent,
    cfg: &Config,
    journal: &Arc<Mutex<Journal>>,
    mut hint_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
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
    let mut terminal = Terminal::with_options(
        CrosstermBackend::new(io::stdout()),
        TerminalOptions {
            viewport: Viewport::Inline(VIEWPORT_HEIGHT),
        },
    )
    .context("初始化行内 TUI 失败")?;
    let mut events = EventStream::new();
    let mut ticker = tokio::time::interval(TICK);

    loop {
        terminal.draw(|frame| app.draw(frame))?;
        let prompt = loop {
            tokio::select! {
                maybe = events.next() => match maybe {
                    Some(Ok(event)) => match app.on_key(event) {
                        Action::Submit(text) => break Some(text),
                        Action::Quit => break None,
                        _ => {}
                    },
                    Some(Err(_)) | None => break None,
                },
                Some(hint) = hint_rx.recv() => app.set_hint(hint),
                _ = ticker.tick() => app.tick(),
            }
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
                maybe = events.next() => {
                    if let Some(Ok(event)) = maybe {
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
    fn agent_events_land_in_the_activity_area() {
        let mut app = new_app();
        app.turn_started("看看仓库");
        app.on_agent_event(AgentEvent::TextDelta("第一行\n第二".into()));
        assert_eq!(app.activity.len(), 2, "换行切开，剩下的是流式尾巴");
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
        let text: Vec<String> = app.activity.iter().map(|line| line.to_string()).collect();
        assert!(text.iter().any(|l| l.contains("› 看看仓库")), "{text:?}");
        assert!(
            text.iter().any(|l| l.contains("✓ Bash(cargo test)")),
            "{text:?}"
        );
        assert!(text.iter().any(|l| l.contains("步")), "{text:?}");
        assert!(!app.running);
    }

    /// 用 TestBackend 验证绘制：活动区、输入行、状态栏与光标位置（不需要真终端）。
    #[test]
    fn draws_activity_input_and_status_bar() {
        use ratatui::{Terminal, backend::TestBackend};

        let mut app = new_app();
        app.turn_started("你好");
        app.on_agent_event(AgentEvent::TextDelta("回答一行\n".into()));
        for c in "接着问".chars() {
            app.on_key(press(KeyCode::Char(c)));
        }

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
        assert!(compact.contains("›你好"), "活动区要有用户消息：{text}");
        assert!(compact.contains("回答一行"), "活动区要有助手回复：{text}");
        assert!(compact.contains("›接着问"), "输入行要有当前输入：{text}");
        assert!(compact.contains("确认模式"), "状态栏要有模式：{text}");
        assert!(compact.contains("deepseek-flash"), "状态栏要有模型：{text}");
        let cursor = terminal.get_cursor_position().unwrap();
        assert_eq!(cursor.y, 10, "光标应在输入行（倒数第二行）：{cursor:?}");
    }

    #[test]
    fn visible_keeps_the_tail_and_the_streaming_line() {
        let mut app = new_app();
        for i in 0..20 {
            app.push_line(format!("第 {i} 行"));
        }
        app.on_agent_event(AgentEvent::TextDelta("尾巴".into()));
        let lines = app.visible(5);
        assert_eq!(lines.len(), 5, "只保留可见的最后几行");
        assert!(lines.last().unwrap().to_string().contains("尾巴"));
    }
}
