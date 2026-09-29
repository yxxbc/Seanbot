//! 行内视口的窗口管理：决定"哪些行留在活动区、哪些行冻结进终端 scrollback"。
//!
//! 分工（设计书 §5）：
//!
//! ```text
//! Terminal Scrollback        ← 已经冻结、由终端自己滚动的历史
//!       ↑ insert_before
//! Frozen Output（屏幕上不可再改的历史行）
//!       ↑
//! Active Viewport（流式内容 / 输入行 / 状态栏 / 浮窗）
//! ```
//!
//! 这个模块只做纯计算（不碰终端、不碰 ratatui），所以可以脱离终端单测；
//! 真正把行写进 scrollback 的动作由主循环拿 `insert_before` 完成。
//!
//! 关键性质：冻结是**单调**的——已经写进 scrollback 的行不会再变（`frozen` 只增不减），
//! 因此不会出现"同一段内容被插入两次"或终端历史重排。

use ratatui::text::Line;

/// 活动区至少保留的行数（输入行 + 状态栏 + 至少一行流式内容）。
pub const MIN_WINDOW: u16 = 4;
/// 活动区最多能占的屏幕比例（分母）：不要吃满整个屏幕，否则冻结内容无处可去。
const MAX_WINDOW_DIVISOR: u16 = 2;
/// 活动区最多能占的绝对行数上限（防止超长终端里活动区吃掉半屏）。
const MAX_WINDOW: u16 = 16;

/// 行内视口的窗口状态。
///
/// 活动区上限 `cap` 必须与 `App::draw` 实际渲染的行数一致：如果冻结得比
/// 实际能画的多，中间的行既不在活动区、又没被冻结，就会**凭空丢掉**（历史缺口）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frozen {
    /// 每帧实际可画的活动区行数（由主循环按帧实际高度同步）
    cap: u16,
    /// 视口宽度
    pub width: u16,
}

impl Default for Frozen {
    fn default() -> Self {
        Self {
            cap: MIN_WINDOW,
            width: 80,
        }
    }
}

impl Frozen {
    /// 按视口总行数与宽度建立窗口状态（进入 TUI 时的初始估计）。
    pub fn new(reserved: u16, width: u16) -> Self {
        let mut frozen = Self {
            cap: MIN_WINDOW,
            width: width.max(1),
        };
        frozen.resize(reserved, width);
        frozen
    }

    /// 活动区最多能显示多少行内容（不含输入行/状态栏这两行）。
    #[cfg(test)]
    pub fn max_lines(&self) -> u16 {
        self.cap
    }

    /// 同步"活动区这一帧实际能画多少行"。
    ///
    /// 传入的必须是 `App::draw` 真正使用的活动区高度——输入框多行时 chrome 会变高，
    /// 用"屏幕高度减固定值"会算出偏大的 cap，让活动区顶部的行既不冻结也不绘制。
    pub fn sync_draw_height(&mut self, active_height: u16) {
        // MAX_WINDOW 一定大于 1（见常量定义），所以 clamp 不会 panic
        self.cap = active_height.clamp(1, MAX_WINDOW);
    }

    /// 屏幕宽度变化：每行的补位宽度跟着变。
    ///
    /// 已经写进终端 scrollback 的行不可能重排（终端接管了历史），所以宽度只影响之后
    /// 冻结的行；`frozen` 的单调性保持不变。
    pub fn resize_width(&mut self, width: u16) {
        self.width = width.max(1);
    }

    /// 终端整体尺寸变化：`reserved` 是新的视口总行数。
    pub fn resize(&mut self, reserved: u16, width: u16) {
        self.width = width.max(1);
        self.cap = (reserved.max(MIN_WINDOW) / MAX_WINDOW_DIVISOR).clamp(MIN_WINDOW, MAX_WINDOW);
    }

    /// 内容超出活动区时，需要冻结的行数。
    ///
    /// `cap` 必须等于 `App::draw` 实际渲染的行数，否则会丢行。
    pub fn freeze_count(&self, lines: usize) -> usize {
        lines.saturating_sub(self.cap.max(1) as usize)
    }
}

/// 把逻辑行折成**屏幕行**：每一行的显示宽度不超过 `width`，并补位到整宽。
///
/// 为什么一定要在这里折：活动区的记账（`freeze_count` 冻结几行、`viewport_window`
/// 取几行）是按行数算的，而终端真正占用的是屏幕行数。若交给渲染层去折（`Wrap`），
/// 一条 48 列的行在 40 列终端里占两个屏幕行，窗口就会把最后一行挤出屏幕（丢内容）。
/// 折完之后"逻辑行 == 屏幕行"，账目才成立。
pub fn wrap_lines(lines: &[Line<'static>], width: u16) -> Vec<Line<'static>> {
    let width = width.max(1) as usize;
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        wrap_line(line, width, &mut out);
    }
    out
}

/// 单行折行：保留 span 样式与对齐，宽字符不会被劈成两半。
fn wrap_line(line: &Line<'static>, width: usize, out: &mut Vec<Line<'static>>) {
    let mut spans: Vec<ratatui::text::Span<'static>> = Vec::new();
    let mut used = 0usize;
    for span in &line.spans {
        for ch in span.content.chars() {
            let cell = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if used + cell > width {
                // 放不下了：收尾这一行（补位到整宽），从新的一行接着写
                push_row(out, line, std::mem::take(&mut spans), used, width);
                used = 0;
            }
            let mut buffer = [0u8; 4];
            spans.push(ratatui::text::Span::styled(
                ch.encode_utf8(&mut buffer).to_string(),
                span.style,
            ));
            used += cell;
        }
    }
    push_row(out, line, spans, used, width);
}

/// 收一行：补空格到整宽（行尾残留会让终端留下上一帧的字符），再推进结果里。
fn push_row(
    out: &mut Vec<Line<'static>>,
    source: &Line<'static>,
    mut spans: Vec<ratatui::text::Span<'static>>,
    used: usize,
    width: usize,
) {
    if used < width {
        spans.push(ratatui::text::Span::raw(" ".repeat(width - used)));
    }
    out.push(Line {
        style: source.style,
        alignment: source.alignment,
        spans,
    });
}

/// 把一段行渲染成 `insert_before` 用的缓冲内容：宽行按宽度截断，保证铺满整行。
pub fn pad_line(line: &Line<'_>, width: u16) -> Line<'static> {
    // 把借用内容拷成 owned，得到 'static 的行（冻结出去的行会被终端长期持有）
    let mut out: Line<'static> = Line {
        style: line.style,
        alignment: line.alignment,
        spans: line
            .spans
            .iter()
            .map(|span| ratatui::text::Span {
                content: span.content.clone().into_owned().into(),
                style: span.style,
            })
            .collect(),
    };
    // 用空格补齐，避免行尾残留上一帧的内容（行内视口上方是终端的真实字符格）
    let used: usize = out
        .spans
        .iter()
        .map(|span| unicode_width::UnicodeWidthStr::width(span.content.as_ref()))
        .sum();
    let width = width as usize;
    if used < width {
        out.spans
            .push(ratatui::text::Span::raw(" ".repeat(width - used)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_follows_the_terminal_size() {
        let f = Frozen::new(24, 80);
        // 视口 24 行 → 活动区上限 12 行
        assert_eq!(f.freeze_count(100), 88);
        assert_eq!(f.freeze_count(12), 0);
    }

    #[test]
    fn freeze_count_is_zero_until_content_overflows() {
        let f = Frozen::new(24, 80);
        assert_eq!(f.freeze_count(5), 0);
        assert_eq!(f.freeze_count(12), 0);
        assert_eq!(f.freeze_count(20), 8);
    }

    #[test]
    fn tiny_terminal_still_keeps_a_usable_window() {
        let f = Frozen::new(5, 40);
        // 5 行视口 → 活动区上限 4 行（MIN_WINDOW 是活动区的下限）
        assert_eq!(f.max_lines(), MIN_WINDOW);
        assert_eq!(f.freeze_count(10), 6);
        assert_eq!(f.freeze_count(MIN_WINDOW as usize), 0);
    }

    #[test]
    fn freeze_decision_does_not_change_with_width() {
        // 活动区上限只跟视口总行数与内容长度有关，重新折行不该改变冻结点
        let mut f = Frozen::new(24, 80);
        let first = f.freeze_count(30);
        f.resize_width(40);
        assert_eq!(f.freeze_count(30), first);
    }

    #[test]
    fn resize_updates_width_and_cap() {
        let mut f = Frozen::new(24, 80);
        f.resize(30, 100);
        assert_eq!(f.width, 100);
        assert_eq!(f.max_lines(), 15);
    }

    #[test]
    fn draw_height_sets_the_active_window_cap() {
        // 关键不变式：活动区上限 == App::draw 实际渲染的行数，否则会丢行
        let mut f = Frozen::new(24, 80);
        f.sync_draw_height(5);
        assert_eq!(f.max_lines(), 5);
        assert_eq!(f.freeze_count(10), 5);
        // 多行输入时活动区变矮，cap 必须跟着降
        f.sync_draw_height(3);
        assert_eq!(f.max_lines(), 3);
        assert_eq!(f.freeze_count(10), 7);
        // 超高时仍受绝对上限约束
        f.sync_draw_height(u16::MAX);
        assert_eq!(f.max_lines(), MAX_WINDOW);
    }

    #[test]
    fn draw_height_never_exceeds_the_absolute_cap() {
        let mut f = Frozen::new(24, 80);
        f.sync_draw_height(u16::MAX);
        assert!(f.max_lines() <= MAX_WINDOW);
    }

    #[test]
    fn freeze_never_drops_a_line() {
        let mut f = Frozen::new(24, 80);
        f.sync_draw_height(5);
        // 冻结的行 + 活动区能画的行 必须覆盖全部内容，一行都不能丢
        for lines in 0..50usize {
            let frozen = f.freeze_count(lines);
            let active = (lines - frozen).min(f.max_lines() as usize);
            assert!(
                frozen + active >= lines,
                "frozen={frozen} active={active} lines={lines}"
            );
        }
    }

    #[test]
    fn wrap_lines_splits_long_text_without_losing_any() {
        let lines = vec![Line::from("abcdefghij")];
        let wrapped = wrap_lines(&lines, 4);
        assert_eq!(wrapped.len(), 3, "10 列折成 4 列一行应当是 3 行");
        let joined: String = wrapped
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("")
            .trim_end()
            .replace(' ', "");
        assert_eq!(joined, "abcdefghij", "不能丢字符也不能多字符");
        for line in &wrapped {
            assert_eq!(
                unicode_width::UnicodeWidthStr::width(line.to_string().as_str()),
                4
            );
        }
    }

    #[test]
    fn wrap_lines_never_splits_a_wide_character() {
        // 5 列放得下两组 CJK（4 列），第三组（2 列）塞不进剩下的 1 列，只能换行——
        // 宁可留一格空白，也不能把宽字符劈成两半
        let wrapped = wrap_lines(&[Line::from("中文中文中文")], 5);
        assert_eq!(wrapped.len(), 3, "{wrapped:?}");
        for line in &wrapped {
            assert_eq!(line.to_string().trim_end(), "中文");
            assert_eq!(
                unicode_width::UnicodeWidthStr::width(line.to_string().as_str()),
                5
            );
        }
    }

    #[test]
    fn wrap_lines_pads_every_row_and_keeps_styles() {
        let styled = Line::from(vec![
            ratatui::text::Span::styled(
                "粗",
                ratatui::style::Style::default().add_modifier(ratatui::style::Modifier::BOLD),
            ),
            ratatui::text::Span::raw("体"),
        ]);
        let wrapped = wrap_lines(&[styled, Line::from("")], 6);
        assert_eq!(wrapped.len(), 2, "空行也要占一行（否则账目又对不上）");
        for line in &wrapped {
            assert_eq!(
                unicode_width::UnicodeWidthStr::width(line.to_string().as_str()),
                6
            );
        }
        assert!(
            wrapped[0].spans[0]
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD),
            "折行不能把样式弄丢"
        );
    }

    #[test]
    fn wrap_lines_is_identity_for_short_lines() {
        let wrapped = wrap_lines(&[Line::from("短")], 80);
        assert_eq!(wrapped.len(), 1);
        assert_eq!(wrapped[0].to_string(), format!("短{}", " ".repeat(78)));
    }

    #[test]
    fn pad_line_fills_to_width() {
        let line = Line::from("ab");
        let padded = pad_line(&line, 5);
        let text: String = padded.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "ab   ");
    }

    #[test]
    fn pad_line_keeps_styles_and_never_truncates() {
        let line = Line::from(vec![
            ratatui::text::Span::styled(
                "bold",
                ratatui::style::Style::default().add_modifier(ratatui::style::Modifier::BOLD),
            ),
            ratatui::text::Span::raw("text"),
        ]);
        let padded = pad_line(&line, 4);
        let text: String = padded.spans.iter().map(|s| s.content.as_ref()).collect();
        // 已经比目标宽就不动它（多余的列由终端裁掉）
        assert_eq!(text, "boldtext");
        assert!(
            padded.spans[0]
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
        );
    }
}
