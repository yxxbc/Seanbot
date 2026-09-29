//! 环环（Loopy）：12x12 像素吉祥物（设计书 §5）。
//!
//! 像素网格只定义"哪里是环"，颜色由该像素相对中心的角度映射到 12 色调色板；
//! 角度偏移随时间变化就成了"流动"的环。渲染用半块字符：上像素当前景色、下像素当背景色，
//! 于是 12 列 x 6 行。

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

/// 环的像素网格（点为透明，其余为环）。
const GRID: [&str; 12] = [
    "....ccgg....",
    "..cc....gg..",
    ".c........g.",
    ".c........go",
    "c..........o",
    "c..........o",
    "r..........o",
    "r..........r",
    ".r........r.",
    ".rr......rr.",
    "..rr....rr..",
    "....rrrr....",
];

/// 12 色调色板：从奶油到珊瑚再回到奶油，环上颜色按角度取。
const PALETTE: [Color; 12] = [
    Color::Rgb(0xF4, 0xE9, 0xD8),
    Color::Rgb(0xEF, 0xD9, 0xA8),
    Color::Rgb(0xE6, 0xB8, 0x5C),
    Color::Rgb(0xE8, 0xA1, 0x55),
    Color::Rgb(0xE8, 0x8A, 0x52),
    Color::Rgb(0xDF, 0x75, 0x50),
    Color::Rgb(0xD9, 0x5F, 0x4B),
    Color::Rgb(0xDF, 0x75, 0x50),
    Color::Rgb(0xE8, 0x8A, 0x52),
    Color::Rgb(0xE8, 0xA1, 0x55),
    Color::Rgb(0xE6, 0xB8, 0x5C),
    Color::Rgb(0xEF, 0xD9, 0xA8),
];

/// 眼白 / 瞳孔 / 闭眼线。
const EYE_WHITE: Color = Color::Rgb(0xF4, 0xE9, 0xD8);
const EYE_PUPIL: Color = Color::Rgb(0xD9, 0x5F, 0x4B);
const EYE_LINE: Color = Color::Rgb(0x1A, 0x1A, 0x22);

/// 环环的表情状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mood {
    /// 待机（欢迎框）：偶尔眨眼
    Idle,
    /// 思考中：渐变流动 + 眼神游移
    Thinking,
    /// 本轮成功：眯眼笑
    Happy,
    /// 出错或中断：叉眼 + 左右晃
    Error,
}

/// 眼神。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eyes {
    Open,
    Blink,
    Left,
    Right,
    Up,
    Happy,
    Cross,
}

impl Eyes {
    /// 眼睛占网格的第 5-8 行、第 5-8 列（4x4）。
    fn map(self) -> [&'static str; 4] {
        match self {
            Self::Open => ["....", "wwww", "pwwp", "...."],
            Self::Blink => ["....", "....", "llll", "...."],
            Self::Left => ["....", "wwww", "ppww", "...."],
            Self::Right => ["....", "wwww", "wwpp", "...."],
            Self::Up => ["....", "pwwp", "wwww", "...."],
            Self::Happy => ["....", "....", "wwww", "...."],
            Self::Cross => ["....", "p..p", ".pp.", "...."],
        }
    }
}

/// 环环的当前帧。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mascot {
    pub mood: Mood,
    pub frame: u64,
}

impl Default for Mascot {
    fn default() -> Self {
        Self {
            mood: Mood::Idle,
            frame: 0,
        }
    }
}

impl Mascot {
    pub fn new(mood: Mood) -> Self {
        Self { mood, frame: 0 }
    }

    /// 前进一帧。
    pub fn tick(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    /// 当前眼神：待机时约 28 帧眨 2 帧；思考时每 5 帧换一次；其它状态固定。
    pub fn eyes(&self) -> Eyes {
        match self.mood {
            Mood::Idle => {
                if self.frame % 28 < 2 {
                    Eyes::Blink
                } else {
                    Eyes::Open
                }
            }
            Mood::Thinking => match (self.frame / 5) % 6 {
                0 => Eyes::Open,
                1 => Eyes::Left,
                2 => Eyes::Open,
                3 => Eyes::Right,
                4 => Eyes::Up,
                _ => Eyes::Open,
            },
            Mood::Happy => Eyes::Happy,
            Mood::Error => Eyes::Cross,
        }
    }

    /// 环的渐变偏移：思考时每帧 +0.04，让颜色流动起来。
    pub fn gradient_offset(&self) -> f32 {
        match self.mood {
            Mood::Thinking => self.frame as f32 * 0.04,
            _ => 0.0,
        }
    }

    /// 出错时的水平晃动（0/1/0/-1）。
    pub fn shake(&self) -> i32 {
        if self.mood != Mood::Error {
            return 0;
        }
        match self.frame % 4 {
            0 => 0,
            1 => 1,
            2 => 0,
            _ => -1,
        }
    }

    /// 12x12 网格上某个像素的颜色（None = 透明）。
    fn pixel(&self, row: usize, col: usize) -> Option<Color> {
        if (4..8).contains(&row) && (4..8).contains(&col) {
            let eyes = self.eyes().map();
            match eyes[row - 4].as_bytes()[col - 4] {
                b'w' => return Some(EYE_WHITE),
                b'p' => return Some(EYE_PUPIL),
                b'l' => return Some(EYE_LINE),
                _ => {}
            }
        }
        let cell = GRID[row].as_bytes()[col];
        if cell == b'.' {
            return None;
        }
        let x = col as f32 + 0.5 - 5.5;
        let y = row as f32 + 0.5 - 5.5;
        let angle = y.atan2(x);
        let normalized = (angle / std::f32::consts::PI + 1.0) / 2.0 + self.gradient_offset();
        let index = (normalized * PALETTE.len() as f32).rem_euclid(PALETTE.len() as f32) as usize;
        Some(PALETTE[index % PALETTE.len()])
    }

    /// 渲染成 6 行（每行 12 列）。
    pub fn lines(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::with_capacity(6);
        for out_row in 0..6 {
            let top_row = out_row * 2;
            let bottom_row = top_row + 1;
            let mut spans: Vec<Span<'static>> = Vec::with_capacity(12);
            for col in 0..12 {
                let top = self.pixel(top_row, col);
                let bottom = self.pixel(bottom_row, col);
                let span = match (top, bottom) {
                    (Some(top), Some(bottom)) => {
                        Span::styled("▀", Style::default().fg(top).bg(bottom))
                    }
                    (Some(top), None) => Span::styled("▀", Style::default().fg(top)),
                    (None, Some(bottom)) => Span::styled("▄", Style::default().fg(bottom)),
                    (None, None) => Span::raw(" "),
                };
                spans.push(span);
            }
            lines.push(Line::from(spans));
        }
        lines
    }

    /// 状态行用的迷你版：转圈字符 + 调色板颜色。
    pub fn mini(&self) -> Span<'static> {
        const FRAMES: [&str; 4] = ["◜", "◝", "◞", "◟"];
        let index = (self.frame / 2) as usize % FRAMES.len();
        let color = PALETTE[(self.frame as usize / 2) % PALETTE.len()];
        Span::styled(
            FRAMES[index],
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )
    }
}

/// 欢迎框（设计书 §4.5）：左侧环环，右侧版本 / 模型 / 目录 / 快捷键。
pub fn welcome_lines(
    mascot: &Mascot,
    version: &str,
    model: &str,
    mode: &str,
    cwd: &str,
) -> Vec<Line<'static>> {
    let right = [
        format!("Seanbot v{version}"),
        format!("{model} · {mode}"),
        cwd.to_string(),
        "/ 查看命令 · ctrl+o 转录 · ctrl+c 退出".to_string(),
    ];
    let art = mascot.lines();
    let width = 46usize;
    let frame_style = Style::default().fg(Color::Rgb(0xE6, 0xB8, 0x5C));
    let mut lines = vec![Line::from(Span::styled(
        format!("┌{}┐", "─".repeat(width)),
        frame_style,
    ))];
    for (index, art_line) in art.iter().enumerate() {
        let mut spans = vec![Span::styled("│ ", frame_style)];
        spans.extend(art_line.spans.iter().cloned());
        let used = 14usize;
        let tail = match right.get(index) {
            Some(text) => format!("  {text}"),
            None => String::new(),
        };
        let pad = width.saturating_sub(used + tail.chars().count());
        spans.push(Span::raw(tail));
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(Span::styled("│", frame_style));
        lines.push(Line::from(spans));
    }
    lines.push(Line::from(Span::styled(
        format!("└{}┘", "─".repeat(width)),
        frame_style,
    )));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(lines: &[Line<'static>]) -> Vec<String> {
        lines.iter().map(|line| line.to_string()).collect()
    }

    #[test]
    fn renders_six_rows_of_half_blocks() {
        let mascot = Mascot::new(Mood::Idle);
        let lines = mascot.lines();
        assert_eq!(lines.len(), 6);
        for line in &lines {
            assert_eq!(line.to_string().chars().count(), 12, "{line:?}");
        }
        let joined = text_of(&lines).join("\n");
        assert!(joined.contains("▀") || joined.contains("▄"), "{joined}");
    }

    #[test]
    fn idle_blinks_every_28_frames() {
        let mut mascot = Mascot::new(Mood::Idle);
        assert_eq!(mascot.eyes(), Eyes::Blink, "第 0 帧眨眼");
        mascot.frame = 2;
        assert_eq!(mascot.eyes(), Eyes::Open);
        mascot.frame = 28;
        assert_eq!(mascot.eyes(), Eyes::Blink, "每 28 帧眨一次");
    }

    #[test]
    fn thinking_cycles_eyes_and_flows() {
        let mut mascot = Mascot::new(Mood::Thinking);
        let mut seen = Vec::new();
        for frame in 0..30 {
            mascot.frame = frame;
            let eyes = mascot.eyes();
            if !seen.contains(&eyes) {
                seen.push(eyes);
            }
        }
        for expected in [Eyes::Open, Eyes::Left, Eyes::Right, Eyes::Up] {
            assert!(
                seen.contains(&expected),
                "思考时应当出现 {expected:?}：{seen:?}"
            );
        }
        mascot.frame = 0;
        let start = mascot.gradient_offset();
        mascot.frame = 10;
        assert!(mascot.gradient_offset() > start, "渐变偏移应当逐帧推进");
    }

    #[test]
    fn happy_and_error_are_distinct() {
        let happy = Mascot::new(Mood::Happy);
        assert_eq!(happy.eyes(), Eyes::Happy);
        assert_eq!(happy.shake(), 0);
        let error = Mascot::new(Mood::Error);
        assert_eq!(error.eyes(), Eyes::Cross);
        let shakes: Vec<i32> = (0..4)
            .map(|frame| Mascot { frame, ..error }.shake())
            .collect();
        assert_eq!(shakes, vec![0, 1, 0, -1], "出错时左右晃");
    }

    #[test]
    fn mini_spinner_cycles_four_frames() {
        let mut mascot = Mascot::new(Mood::Thinking);
        let mut seen = Vec::new();
        for frame in 0..8 {
            mascot.frame = frame;
            let text = mascot.mini().content.to_string();
            if !seen.contains(&text) {
                seen.push(text);
            }
        }
        assert_eq!(seen.len(), 4, "迷你转圈有四种字形：{seen:?}");
    }

    #[test]
    fn welcome_box_carries_version_model_mode_and_cwd() {
        let mascot = Mascot::new(Mood::Idle);
        let lines = welcome_lines(&mascot, "0.1.2", "deepseek-flash", "确认模式", "~/proj");
        let text = text_of(&lines).join("\n");
        for needle in [
            "Seanbot v0.1.2",
            "deepseek-flash",
            "确认模式",
            "~/proj",
            "ctrl+o",
        ] {
            assert!(text.contains(needle), "欢迎框缺少 {needle}：{text}");
        }
        assert!(text.starts_with("┌"), "{text}");
        assert!(text.trim_end().ends_with("┘"), "{text}");
        assert_eq!(lines.len(), 8, "上下边框 + 6 行环环");
    }
}
