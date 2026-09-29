//! 把 AgentEvent 渲染到终端。

use std::{
    collections::HashMap,
    io::{self, IsTerminal, Write},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crossterm::{
    cursor, queue,
    style::Stylize,
    terminal::{self, ClearType},
};
use seanbot_core::AgentEvent;
use tokio::{sync::mpsc, time::MissedTickBehavior};

use crate::format;

pub(crate) const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const PREVIEW_LINES: usize = 3;
const PREVIEW_WIDTH: usize = 120;
const TICK: Duration = Duration::from_millis(80);

pub type Clock = Box<dyn Fn() -> Instant + Send>;

/// 渲染器跨任务共享：渲染任务画动画，权限确认临时接管终端。
pub type SharedRenderer<W> = Arc<Mutex<Renderer<W>>>;

pub fn shared<W: Write + Send>(renderer: Renderer<W>) -> SharedRenderer<W> {
    Arc::new(Mutex::new(renderer))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderStyle {
    pub animate: bool,
    pub color: bool,
    pub show_reasoning: bool,
    /// 终端列数；`None` 表示每次绘制时查询终端尺寸。
    pub cols: Option<u16>,
}

impl RenderStyle {
    /// stdout 是终端且未设置 NO_COLOR 时开启动画与颜色。
    pub fn detect(show_reasoning: bool) -> Self {
        let fancy = io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
        Self {
            animate: fancy,
            color: fancy,
            show_reasoning,
            cols: None,
        }
    }
}

enum Spinner {
    Thinking(Instant),
    Tool { label: String, since: Instant },
}

pub struct Renderer<W: Write> {
    out: W,
    style: RenderStyle,
    clock: Clock,
    spinner: Option<Spinner>,
    spinner_drawn: bool,
    frame: usize,
    thinking_since: Option<Instant>,
    reasoned: bool,
    reasoning_open: bool,
    at_line_start: bool,
    labels: HashMap<String, String>,
    /// 终端被外部接管（如权限确认）时暂停动画
    paused: bool,
    /// 本轮 Markdown 流式渲染器；仅在终端（`style.color`）时启用，重定向时保持原始 Markdown
    markdown: Option<crate::markdown::Streaming>,
}

impl<W: Write> Renderer<W> {
    pub fn new(out: W, style: RenderStyle, clock: Clock) -> Self {
        Self {
            out,
            style,
            clock,
            spinner: None,
            spinner_drawn: false,
            frame: 0,
            thinking_since: None,
            reasoned: false,
            reasoning_open: false,
            at_line_start: true,
            labels: HashMap::new(),
            paused: false,
            markdown: None,
        }
    }

    #[cfg(test)]
    pub fn into_inner(self) -> W {
        self.out
    }

    pub fn is_animating(&self) -> bool {
        self.style.animate && self.spinner.is_some() && !self.paused
    }

    /// 交出终端：停掉动画并擦掉动画行，之后由调用方直接往 stdout 写。
    pub fn suspend(&mut self) -> io::Result<()> {
        self.paused = true;
        self.stop_spinner()?;
        self.out.flush()
    }

    /// 收回终端；动画会在下一次 tick 重画。
    pub fn resume(&mut self) {
        self.paused = false;
    }

    pub fn handle(&mut self, event: AgentEvent) -> io::Result<()> {
        match event {
            AgentEvent::MessageAppended(_) | AgentEvent::MessageRetracted => {}
            AgentEvent::ThinkingStarted => {
                let now = (self.clock)();
                self.thinking_since = Some(now);
                self.reasoned = false;
                if self.style.animate {
                    self.spinner = Some(Spinner::Thinking(now));
                    self.draw_spinner()?;
                }
            }
            AgentEvent::ReasoningDelta(text) => {
                self.reasoned = true;
                if self.style.show_reasoning {
                    self.stop_spinner()?;
                    if !self.reasoning_open {
                        self.ensure_newline()?;
                        self.reasoning_open = true;
                    }
                    let styled = self.dim(&text);
                    self.write_raw(&styled, &text)?;
                }
            }
            AgentEvent::TextDelta(text) => {
                self.end_thinking()?;
                if self.style.color {
                    // 终端里流式渲染 Markdown：冻结的行立刻写出，其余留给活动区域
                    if self.markdown.is_none() {
                        let width = self.cols();
                        self.markdown = Some(crate::markdown::Streaming::new(width));
                    }
                    let lines = match self.markdown.as_mut() {
                        Some(stream) => stream.push(&text),
                        None => Vec::new(),
                    };
                    if !lines.is_empty() {
                        let ansi = crate::markdown::lines_to_ansi(&lines);
                        self.write_ansi(&ansi)?;
                    }
                } else {
                    self.write_raw(&text, &text)?;
                }
            }
            AgentEvent::ToolStarted {
                call_id,
                name,
                title,
            } => {
                self.end_thinking()?;
                self.flush_markdown()?;
                self.ensure_newline()?;
                let label = format::tool_label(&name, &title);
                if self.style.animate {
                    self.spinner = Some(Spinner::Tool {
                        label: label.clone(),
                        since: (self.clock)(),
                    });
                    self.draw_spinner()?;
                }
                self.labels.insert(call_id, label);
            }
            AgentEvent::ToolFinished {
                call_id,
                ok,
                summary,
                preview,
                elapsed,
            } => {
                self.stop_spinner()?;
                self.ensure_newline()?;
                let label = self.labels.remove(&call_id).unwrap_or(call_id);
                let mark = if ok {
                    self.paint_ok("✓")
                } else {
                    self.paint_err("✗")
                };
                self.line(&format!("{mark} {label} · {}", format::secs(elapsed)))?;
                let body = if !preview.is_empty() {
                    preview
                } else if !summary.is_empty() {
                    vec![summary]
                } else {
                    Vec::new()
                };
                self.write_preview(&body)?;
            }
            AgentEvent::TurnFinished { usage, steps } => {
                self.end_thinking()?;
                self.finish_line()?;
                let s = self.dim(&format::usage_line(usage.as_ref(), steps));
                self.line(&s)?;
            }
            AgentEvent::Cancelled => {
                self.end_thinking()?;
                self.finish_line()?;
                let s = self.paint_warn("⎿ 已中断");
                self.line(&s)?;
            }
            AgentEvent::Error(msg) => {
                self.end_thinking()?;
                self.finish_line()?;
                let s = self.paint_err(&format!("✗ 错误：{msg}"));
                self.line(&s)?;
            }
        }
        self.out.flush()
    }

    pub fn tick(&mut self) -> io::Result<()> {
        if self.is_animating() {
            self.frame = (self.frame + 1) % FRAMES.len();
            self.draw_spinner()?;
            self.out.flush()?;
        }
        Ok(())
    }

    /// 思考阶段结束：清掉动画，收束推理输出，必要时输出 "Thought for"。
    fn end_thinking(&mut self) -> io::Result<()> {
        let Some(since) = self.thinking_since.take() else {
            return Ok(());
        };
        self.stop_spinner()?;
        if self.reasoning_open {
            self.reasoning_open = false;
            self.ensure_newline()?;
        }
        if self.reasoned {
            self.ensure_newline()?;
            let s = self.dim(&format!(
                "✻ Thought for {}",
                format::secs((self.clock)() - since)
            ));
            self.line(&s)?;
        }
        Ok(())
    }

    fn finish_line(&mut self) -> io::Result<()> {
        self.flush_markdown()?;
        self.stop_spinner()?;
        self.ensure_newline()
    }

    /// 终端宽度（Markdown 表格折行用）。
    fn cols(&self) -> usize {
        self.style
            .cols
            .map(usize::from)
            .or_else(|| {
                crossterm::terminal::size()
                    .ok()
                    .map(|(cols, _)| cols as usize)
            })
            .unwrap_or(80)
    }

    /// 结束本轮 Markdown 流式渲染，把还没写出的行写掉。
    fn flush_markdown(&mut self) -> io::Result<()> {
        let Some(mut stream) = self.markdown.take() else {
            return Ok(());
        };
        let lines = stream.finish();
        if !lines.is_empty() {
            let ansi = crate::markdown::lines_to_ansi(&lines);
            self.write_ansi(&ansi)?;
        }
        Ok(())
    }

    /// 直接写已经带转义序列的内容（不再套样式）。
    fn write_ansi(&mut self, text: &str) -> io::Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        self.out.write_all(text.as_bytes())?;
        self.at_line_start = text.ends_with('\n');
        Ok(())
    }

    fn write_preview(&mut self, body: &[String]) -> io::Result<()> {
        for (i, raw) in body.iter().take(PREVIEW_LINES).enumerate() {
            let text = self.dim(&format::clip(&format::sanitize(raw), PREVIEW_WIDTH));
            let prefix = if i == 0 { "  ⎿ " } else { "    " };
            self.line(&format!("{prefix}{text}"))?;
        }
        if body.len() > PREVIEW_LINES {
            let more = self.dim(&format!("(+{} 行)", body.len() - PREVIEW_LINES));
            self.line(&format!("    {more}"))?;
        }
        Ok(())
    }

    fn write_raw(&mut self, styled: &str, plain: &str) -> io::Result<()> {
        if plain.is_empty() {
            return Ok(());
        }
        write!(self.out, "{styled}")?;
        self.at_line_start = plain.ends_with('\n');
        Ok(())
    }

    fn line(&mut self, s: &str) -> io::Result<()> {
        writeln!(self.out, "{s}")?;
        self.at_line_start = true;
        Ok(())
    }

    fn ensure_newline(&mut self) -> io::Result<()> {
        if !self.at_line_start {
            self.line("")?;
        }
        Ok(())
    }

    fn draw_spinner(&mut self) -> io::Result<()> {
        let now = (self.clock)();
        let text = match &self.spinner {
            None => return Ok(()),
            Some(Spinner::Thinking(since)) => format!("✻ Thinking… {}", format::secs(now - *since)),
            Some(Spinner::Tool { label, since }) => {
                format!(
                    "{} {label} {}",
                    FRAMES[self.frame],
                    format::secs(now - *since)
                )
            }
        };
        let cols = self
            .style
            .cols
            .or_else(|| terminal::size().ok().map(|(c, _)| c))
            .unwrap_or(80) as usize;
        // 动画行必须短于终端宽度，否则折行后"清行"只清最后一行，画面会被刷满
        let text = format::clip_width(&text, cols.saturating_sub(1).max(10));
        self.ensure_newline()?;
        let styled = self.dim(&text);
        queue!(
            self.out,
            cursor::MoveToColumn(0),
            terminal::Clear(ClearType::CurrentLine)
        )?;
        write!(self.out, "{styled}")?;
        self.spinner_drawn = true;
        Ok(())
    }

    /// 擦掉动画行并停止动画。
    fn stop_spinner(&mut self) -> io::Result<()> {
        self.spinner = None;
        if self.spinner_drawn {
            queue!(
                self.out,
                cursor::MoveToColumn(0),
                terminal::Clear(ClearType::CurrentLine)
            )?;
            self.spinner_drawn = false;
        }
        Ok(())
    }

    fn dim(&self, s: &str) -> String {
        if self.style.color {
            s.dark_grey().to_string()
        } else {
            s.to_string()
        }
    }

    fn paint_ok(&self, s: &str) -> String {
        if self.style.color {
            s.green().to_string()
        } else {
            s.to_string()
        }
    }

    fn paint_err(&self, s: &str) -> String {
        if self.style.color {
            s.red().to_string()
        } else {
            s.to_string()
        }
    }

    fn paint_warn(&self, s: &str) -> String {
        if self.style.color {
            s.yellow().to_string()
        } else {
            s.to_string()
        }
    }
}

/// 在独立任务中消费事件并按 80ms 刷新动画；发送端关闭后返回。
pub async fn drive<W: Write + Send>(
    renderer: SharedRenderer<W>,
    mut rx: mpsc::Receiver<AgentEvent>,
) {
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        let animating = renderer.lock().unwrap().is_animating();
        tokio::select! {
            event = rx.recv() => match event {
                Some(e) => {
                    let _ = renderer.lock().unwrap().handle(e);
                }
                None => break,
            },
            _ = ticker.tick(), if animating => {
                let _ = renderer.lock().unwrap().tick();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seanbot_provider::Usage;

    /// 终端（color=true）时正文走 Markdown 渲染；重定向时保持原始 Markdown。
    #[test]
    fn terminal_renders_markdown_and_redirect_stays_raw() {
        let t0 = Instant::now();
        let make = |color: bool| {
            let style = RenderStyle {
                animate: false,
                color,
                show_reasoning: false,
                cols: Some(80),
            };
            Renderer::new(Vec::new(), style, Box::new(move || t0))
        };

        let mut fancy = make(true);
        fancy.handle(AgentEvent::ThinkingStarted).unwrap();
        fancy
            .handle(AgentEvent::TextDelta("- 一\n- 二\n".into()))
            .unwrap();
        fancy.handle(AgentEvent::Cancelled).unwrap();
        let out = String::from_utf8(fancy.into_inner()).unwrap();
        assert!(out.contains("• 一"), "终端里应当渲染出列表：{out:?}");
        assert!(!out.contains("- 一"), "列表标记不该原样出现：{out:?}");

        let mut plain = make(false);
        plain.handle(AgentEvent::ThinkingStarted).unwrap();
        plain
            .handle(AgentEvent::TextDelta("- 一\n".into()))
            .unwrap();
        plain.handle(AgentEvent::Cancelled).unwrap();
        let out = String::from_utf8(plain.into_inner()).unwrap();
        assert!(out.contains("- 一"), "重定向时应当原样输出：{out:?}");
        assert!(!out.contains("• 一"), "重定向时不该渲染：{out:?}");
    }

    fn render(events: Vec<AgentEvent>, show_reasoning: bool) -> String {
        let t0 = Instant::now();
        let style = RenderStyle {
            animate: false,
            color: false,
            show_reasoning,
            cols: Some(80),
        };
        let mut r = Renderer::new(Vec::new(), style, Box::new(move || t0));
        for e in events {
            r.handle(e).unwrap();
        }
        String::from_utf8(r.into_inner()).unwrap()
    }

    fn started(name: &str, title: &str) -> AgentEvent {
        AgentEvent::ToolStarted {
            call_id: "c1".into(),
            name: name.into(),
            title: title.into(),
        }
    }

    fn finished(ok: bool, summary: &str, preview: &[&str], ms: u64) -> AgentEvent {
        AgentEvent::ToolFinished {
            call_id: "c1".into(),
            ok,
            summary: summary.into(),
            preview: preview.iter().map(|s| s.to_string()).collect(),
            elapsed: Duration::from_millis(ms),
        }
    }

    #[test]
    fn plain_conversation() {
        let usage = Usage {
            input_tokens: 12_300,
            output_tokens: 420,
            cache_hit_tokens: Some(11_800),
            cache_miss_tokens: Some(500),
        };
        let out = render(
            vec![
                AgentEvent::ThinkingStarted,
                AgentEvent::TextDelta("你好".into()),
                AgentEvent::TextDelta("，世界".into()),
                AgentEvent::TurnFinished {
                    usage: Some(usage),
                    steps: 1,
                },
            ],
            false,
        );
        insta::assert_snapshot!(out.trim_end(), @r"
        你好，世界
        ↑12.3k (缓存 11.8k) ↓420 · 1 步
        ");
    }

    #[test]
    fn thought_line_when_reasoning_hidden() {
        let out = render(
            vec![
                AgentEvent::ThinkingStarted,
                AgentEvent::ReasoningDelta("想一想".into()),
                AgentEvent::TextDelta("答案".into()),
                AgentEvent::TurnFinished {
                    usage: None,
                    steps: 1,
                },
            ],
            false,
        );
        insta::assert_snapshot!(out.trim_end(), @r"
        ✻ Thought for 0.0s
        答案
        1 步
        ");
    }

    #[test]
    fn reasoning_streamed_when_enabled() {
        let out = render(
            vec![
                AgentEvent::ThinkingStarted,
                AgentEvent::ReasoningDelta("想一想".into()),
                AgentEvent::TextDelta("答案".into()),
                AgentEvent::TurnFinished {
                    usage: None,
                    steps: 1,
                },
            ],
            true,
        );
        insta::assert_snapshot!(out.trim_end(), @r"
        想一想
        ✻ Thought for 0.0s
        答案
        1 步
        ");
    }

    #[test]
    fn tool_success_with_folded_preview() {
        let out = render(
            vec![
                AgentEvent::ThinkingStarted,
                started("bash", "cargo build"),
                finished(
                    true,
                    "退出码 0",
                    &["Compiling a", "Compiling b", "Finished", "x", "y"],
                    2300,
                ),
                AgentEvent::ThinkingStarted,
                AgentEvent::TextDelta("构建成功".into()),
                AgentEvent::TurnFinished {
                    usage: None,
                    steps: 2,
                },
            ],
            false,
        );
        insta::assert_snapshot!(out.trim_end(), @r"
        ✓ Bash(cargo build) · 2.3s
          ⎿ Compiling a
            Compiling b
            Finished
            (+2 行)
        构建成功
        2 步
        ");
    }

    #[test]
    fn tool_failure_shows_summary() {
        let out = render(
            vec![
                started("edit", "src/main.rs"),
                finished(false, "old_string 在文件中出现 3 次", &[], 100),
            ],
            false,
        );
        insta::assert_snapshot!(out.trim_end(), @r"
        ✗ Edit(src/main.rs) · 0.1s
          ⎿ old_string 在文件中出现 3 次
        ");
    }

    #[test]
    fn text_then_tool_starts_on_new_line() {
        let out = render(
            vec![
                AgentEvent::TextDelta("我先看看".into()),
                started("read", "a.txt"),
                finished(true, "读取 10 行", &[], 0),
            ],
            false,
        );
        insta::assert_snapshot!(out.trim_end(), @r"
        我先看看
        ✓ Read(a.txt) · 0.0s
          ⎿ 读取 10 行
        ");
    }

    #[test]
    fn cancelled_turn() {
        let out = render(
            vec![
                AgentEvent::ThinkingStarted,
                AgentEvent::TextDelta("写到一半".into()),
                AgentEvent::Cancelled,
            ],
            false,
        );
        insta::assert_snapshot!(out.trim_end(), @r"
        写到一半
        ⎿ 已中断
        ");
    }

    #[test]
    fn error_line() {
        let out = render(
            vec![
                AgentEvent::ThinkingStarted,
                AgentEvent::Error("认证失败".into()),
            ],
            false,
        );
        insta::assert_snapshot!(out.trim_end(), @"✗ 错误：认证失败");
    }

    #[test]
    fn preview_is_sanitized() {
        let out = render(
            vec![
                started("bash", "x"),
                finished(true, "", &["\x1b[32mCompiling\x1b[0m\tfoo"], 0),
            ],
            false,
        );
        assert_eq!(out, "✓ Bash(x) · 0.0s\n  ⎿ Compiling    foo\n");
    }

    #[test]
    fn spinner_fits_terminal_width() {
        let t0 = Instant::now();
        let style = RenderStyle {
            animate: true,
            color: false,
            show_reasoning: false,
            cols: Some(20),
        };
        let mut r = Renderer::new(Vec::new(), style, Box::new(move || t0));
        r.handle(started(
            "bash",
            "cargo test --workspace -- --nocapture 很长的中文参数",
        ))
        .unwrap();
        r.tick().unwrap();
        let out = String::from_utf8(r.into_inner()).unwrap();
        // 最后一次重绘在最后一个"清行"序列之后
        let last = out.rsplit("\x1b[2K").next().unwrap();
        let width = unicode_width::UnicodeWidthStr::width(format::sanitize(last).as_str());
        assert!(width <= 19, "动画行宽 {width}：{last:?}");
        assert!(last.contains('…'));
    }

    #[test]
    fn animated_mode_draws_spinner_and_color() {
        let t0 = Instant::now();
        let style = RenderStyle {
            animate: true,
            color: true,
            show_reasoning: false,
            cols: Some(80),
        };
        let mut r = Renderer::new(Vec::new(), style, Box::new(move || t0));
        r.handle(AgentEvent::ThinkingStarted).unwrap();
        assert!(r.is_animating());
        r.tick().unwrap();
        r.handle(AgentEvent::TextDelta("x".into())).unwrap();
        assert!(!r.is_animating());
        r.handle(AgentEvent::TurnFinished {
            usage: None,
            steps: 1,
        })
        .unwrap();
        let out = String::from_utf8(r.into_inner()).unwrap();
        assert!(out.contains("✻ Thinking… 0.0s"));
        assert!(out.contains("\x1b["));
        assert!(out.contains('x'));
    }
}
