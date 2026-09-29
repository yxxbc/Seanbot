//! 终端生命周期：进入/退出行内 TUI，以及保证"无论如何都还原终端"的守卫。
//!
//! Inline TUI 的前提是 `Viewport::Inline`——只有行内视口才能把已完成输出通过
//! `insert_before` 推进终端 scrollback（`Terminal::insert_before` 对 Fullscreen/Fixed
//! 视口是空操作）。行内视口的锚点要靠 CPR（光标位置查询）确定，所以：
//!
//! - 全程只有一个 stdin 读取者（主循环里的 `poll_event`），不另起 `EventStream` 读线程；
//! - CPR 查询只在进入时发生一次（`Terminal::with_options` 内部）；
//! - 非 TTY（管道、CI、测试）不进行内视图口，由调用方退回全屏或纯文本模式。
//!
//! 还原终端有三道保险：`Session` 的 `Drop`、panic hook、以及 `Guard`（在没有终端
//! 会话时单独使用，例如 TestBackend 测试）。

use std::io::{self, IsTerminal, Write};

use crossterm::{
    cursor::{self, MoveTo},
    event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, TerminalOptions, Viewport, backend::CrosstermBackend};

/// 进入/退出行内 TUI 时要还原的终端能力。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Options {
    /// 是否捕获鼠标（滚轮/选择由 `ui.mouse` 决定）
    pub mouse: bool,
}

/// 终端守卫：只负责 raw mode + bracketed paste（+ 可选鼠标）的进出与 panic 兜底。
///
/// 单独使用它时（TestBackend 测试、无行内视口的场景）由调用方自己管 AlternateScreen。
pub struct Guard;

impl Guard {
    /// 进入 raw mode 与 bracketed paste，并装上 panic hook。
    pub fn enter(options: Options) -> io::Result<Self> {
        enter_raw(options)?;
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // 先把终端还原，再把 panic 交给原来的钩子（保证错误信息还能看到）
            let _ = restore_terminal();
            previous(info);
        }));
        Ok(Self)
    }

    /// 当前是否在终端里；非 TTY 时行内视口没有意义。
    pub fn is_tty() -> bool {
        io::stdout().is_terminal() && io::stdin().is_terminal()
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = restore_terminal();
    }
}

/// 行内 TUI 的终端会话：`Viewport::Inline` + 终端守卫。
///
/// 视口高度在进入时固定（活动区上限），实际每帧渲染多少行由 `App` 决定，剩下的行留白。
/// 终端尺寸变化由主循环感知，`App` 的活动区上限会跟着帧高度走。
pub struct Session {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    guard: Option<Guard>,
}

impl Session {
    /// 进入行内 TUI。返回 `None` 表示当前不是 TTY，调用方应退回非交互路径。
    pub fn enter(rows: u16, options: Options) -> io::Result<Option<Self>> {
        if !Guard::is_tty() {
            return Ok(None);
        }
        let (_, screen_rows) = crossterm::terminal::size()?;
        // 至少给 scrollback 留两行，否则视口贴满屏幕、冻结内容无处可去
        let rows = rows.max(3).min(screen_rows.saturating_sub(2).max(3));

        // Viewport::Inline 会在构造时读一次 CPR（光标位置），此时还没有别的 stdin 读取者
        let guard = Guard::enter(options)?;
        let terminal = inline_terminal(CrosstermBackend::new(io::stdout()), rows)?;
        Ok(Some(Self {
            terminal,
            guard: Some(guard),
        }))
    }

    pub fn terminal(&mut self) -> &mut Terminal<CrosstermBackend<io::Stdout>> {
        &mut self.terminal
    }

    /// 退出行内 TUI：把光标放到视口下方新的一行，让壳提示符接着打印。
    ///
    /// 这里**不进备用屏幕**，所以交付给 shell 的是完整的终端 scrollback 历史。
    pub fn finish(&mut self) -> io::Result<()> {
        let area = self.terminal.get_frame().area();
        let _ = self.terminal.show_cursor();
        // 退出后 shell 提示符应出现在活动区下方，而不是盖住最后一帧
        let _ = execute!(io::stdout(), MoveTo(0, area.bottom()));
        let _ = execute!(io::stdout(), cursor::Show);
        self.guard.take();
        let _ = restore_terminal();
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // 还没显式 finish 就析构（panic、提前 return）：至少别把 raw mode 留下来
        if self.guard.take().is_some() {
            let _ = restore_terminal();
        }
    }
}

/// 用行内视口构造终端；视口高度在构造时固定。
pub fn inline_terminal<B: ratatui::backend::Backend>(
    backend: B,
    rows: u16,
) -> io::Result<Terminal<B>> {
    Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(rows.max(3)),
        },
    )
}

/// 进入 raw mode / bracketed paste（可选鼠标）。
fn enter_raw(options: Options) -> io::Result<()> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnableBracketedPaste)?;
    if options.mouse {
        // 失败不影响主流程：鼠标只是锦上添花
        let _ = execute!(out, EnableMouseCapture);
    }
    out.flush()?;
    Ok(())
}

/// 还原终端：关闭鼠标捕获 / bracketed paste、显示光标、退出 raw mode。
pub fn restore_terminal() -> io::Result<()> {
    let mut out = io::stdout();
    // 任何一步失败都不能让后面几步被跳过——终端还原必须尽力做全
    let _ = restore_sequence(&mut out);
    let _ = disable_raw_mode();
    let _ = out.flush();
    Ok(())
}

/// 还原序列的转义码部分（不含 raw mode，因为那是 termios 调用而不是写字节）。
///
/// 抽成独立函数是为了能在没有 TTY 的环境里断言"到底写了哪些还原指令"。
///
/// **每一步都尽力而为**：某一步失败不能让后面的还原被跳过（否则退出后 bracketed paste
/// 还开着）。Windows 上关鼠标捕获走 winapi、需要之前保存过控制台模式，进程没开过鼠标
/// 捕获时会报 `Initial console modes not set`——这时剩下的两步照样要写完。
fn restore_sequence(out: &mut impl Write) -> io::Result<()> {
    // `and` 会跑完三步再返回第一个错误（而不是短路），顺序也保持"先鼠标、再粘贴、后光标"
    execute!(out, DisableMouseCapture)
        .and(execute!(out, DisableBracketedPaste))
        .and(execute!(out, cursor::Show))
}

/// 进入备用屏幕（仅在行内视口不可用时作为兜底路径使用）。
pub fn enter_alternate() -> io::Result<()> {
    execute!(io::stdout(), EnterAlternateScreen)?;
    Ok(())
}

/// 退出备用屏幕。
pub fn leave_alternate() -> io::Result<()> {
    execute!(io::stdout(), LeaveAlternateScreen)?;
    Ok(())
}

/// 测试用：直接拿一个 TestBackend 的行内终端，不走 raw mode / CPR。
#[cfg(test)]
pub fn test_terminal(
    width: u16,
    height: u16,
    rows: u16,
) -> Terminal<ratatui::backend::TestBackend> {
    inline_terminal(ratatui::backend::TestBackend::new(width, height), rows).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_default_does_not_capture_mouse() {
        assert!(!Options::default().mouse);
    }

    #[test]
    fn restore_terminal_is_idempotent_without_a_tty() {
        // 没有 TTY 时这些 crossterm 调用应当安静地失败而不是 panic
        assert!(restore_terminal().is_ok());
        assert!(restore_terminal().is_ok());
    }

    #[test]
    fn restore_writes_every_needed_escape_sequence() {
        // 终端还原的每一项都不能漏：漏掉任何一条都会留下"坏掉的终端"。
        // 每一步都是尽力而为，所以这里不看返回值，只看到底写了哪些指令。
        let mut out: Vec<u8> = Vec::new();
        let _ = restore_sequence(&mut out);
        let text = String::from_utf8(out).unwrap();
        let mut expected = vec![
            ("关闭 bracketed paste", "\u{1b}[?2004l"),
            ("恢复光标显示", "\u{1b}[?25h"),
        ];
        // Windows 上关鼠标捕获走 winapi（要先保存过控制台模式），往内存里写不产生转义码：
        // 这条序列只在非 Windows 上断言，Windows 的行为由下面那个"失败不跳过"的测试守
        if !cfg!(windows) {
            expected.push(("关闭鼠标捕获", "\u{1b}[?1000l"));
        }
        for (name, code) in expected {
            assert!(text.contains(code), "还原序列缺少「{name}」：{text:?}");
        }
    }

    /// 模拟"第一次写就失败"的 writer（Windows 上关鼠标捕获报错就是这种效果）。
    struct FailsOnce {
        bytes: Vec<u8>,
        failed: bool,
    }

    impl Write for FailsOnce {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.failed {
                self.bytes.extend_from_slice(buf);
                return Ok(buf.len());
            }
            self.failed = true;
            Err(io::Error::other("模拟写失败"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// 某一步写失败（Windows 上关鼠标捕获就会这样）不能让后面的还原被跳过。
    ///
    /// 这里只断言**最后一步（恢复光标）照样写了**：失败具体落在哪一步与平台有关——
    /// Windows 上关鼠标捕获走 winapi、根本不写字节，模拟的"第一次写失败"就落到
    /// bracketed paste 那一步；非 Windows 则落在鼠标那一步。两种情况都必须走完。
    #[test]
    fn a_failed_step_does_not_skip_the_rest_of_the_restore() {
        let mut out = FailsOnce {
            bytes: Vec::new(),
            failed: false,
        };
        let result = restore_sequence(&mut out);
        assert!(result.is_err(), "写失败要能被看到，不能被吞掉");
        let text = String::from_utf8(out.bytes).unwrap();
        assert!(
            text.contains("\u{1b}[?25h"),
            "某一步失败后，恢复光标显示仍然要写出去：{text:?}"
        );
        if !cfg!(windows) {
            assert!(
                text.contains("\u{1b}[?2004l"),
                "非 Windows 上第一步就失败，后面的 bracketed paste 也要写：{text:?}"
            );
        }
    }

    #[test]
    fn guard_reports_non_tty_in_tests() {
        // 测试环境没有 TTY：Session::enter 必须返回 None 而不是去读 CPR
        assert!(Session::enter(6, Options::default()).unwrap().is_none());
    }
}
