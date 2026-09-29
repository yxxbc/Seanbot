//! 行内视口的行为回归测试。
//!
//! 这些测试直接对着 ratatui 的 `TestBackend`：它实现了 `scroll_region_up/down`
//! （见 ratatui 的 `scrolling-regions` feature），所以"冻结的行滚进终端 scrollback"
//! 这件事可以在没有真实终端的情况下断言——不需要 pty。

use ratatui::{
    Terminal,
    backend::TestBackend,
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

const INLINE_HEIGHT: u16 = 3;
const SCREEN: u16 = 12;
const WIDTH: u16 = 40;

fn line(text: &str) -> Line<'static> {
    Line::from(Span::raw(format!("{text:<width$}", width = WIDTH as usize)))
}

fn scrollback(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let sb = terminal.backend().scrollback();
    (0..sb.area.height)
        .map(|y| {
            let row: String = (0..WIDTH)
                .map(|x| sb[(x, y)].symbol().to_string())
                .collect();
            row.trim_end().to_string()
        })
        .collect()
}

#[test]
fn freezing_past_the_screen_bottom_rolls_lines_into_scrollback() {
    let backend = TestBackend::new(WIDTH, SCREEN);
    let mut terminal = Terminal::with_options(
        backend,
        ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Inline(INLINE_HEIGHT),
        },
    )
    .unwrap();

    // 每冻结一行就重绘一次，模拟逐行把完成输出推进终端的节奏
    for index in 0..12 {
        terminal
            .insert_before(1, |buf| {
                Paragraph::new(line(&format!("FROZEN-{index:02}"))).render(buf.area, buf);
            })
            .unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(Paragraph::new(format!("ACTIVE-{index:02}")), frame.area());
            })
            .unwrap();
    }

    // 屏幕上放不下的最早几行必须进入终端 scrollback，且顺序不乱
    let sb = scrollback(&terminal);
    assert!(!sb.is_empty(), "内容超出屏幕后必须有行滚进 scrollback");
    for (index, row) in sb.iter().enumerate() {
        assert_eq!(
            row,
            &format!("FROZEN-{index:02}"),
            "第 {index} 行错位: {sb:?}"
        );
    }

    // 活动区仍然贴在自己那一块（行内视口没有吃满整个屏幕）
    let area = terminal.get_frame().area();
    assert_eq!(area.height, INLINE_HEIGHT);
    assert!(area.bottom() <= SCREEN);
}

#[test]
fn full_screen_viewport_still_supports_insert_before() {
    // 视口刚好等于屏幕高度是一种边界情况：ratatui 会借用首行再滚出去
    let backend = TestBackend::new(WIDTH, 6);
    let mut terminal = Terminal::with_options(
        backend,
        ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Inline(6),
        },
    )
    .unwrap();
    for index in 0..3 {
        terminal
            .insert_before(1, |buf| {
                Paragraph::new(line(&format!("EDGE-{index}"))).render(buf.area, buf);
            })
            .unwrap();
        terminal
            .draw(|frame| frame.render_widget(Paragraph::new("X"), frame.area()))
            .unwrap();
    }
    let sb = scrollback(&terminal);
    assert_eq!(sb[0], "EDGE-0", "scrollback = {sb:?}");
}
