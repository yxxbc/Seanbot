//! 终端输出的格式化助手。

use std::{path::Path, time::Duration};

use seanbot_provider::Usage;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// 12300 → "12.3k"，128000 → "128k"，1000000 → "1M"。
pub fn tokens(n: u64) -> String {
    let (value, unit) = if n >= 1_000_000 {
        (n as f64 / 1e6, "M")
    } else if n >= 1000 {
        (n as f64 / 1e3, "k")
    } else {
        return n.to_string();
    };
    let s = format!("{value:.1}");
    format!("{}{unit}", s.strip_suffix(".0").unwrap_or(&s))
}

pub fn secs(d: Duration) -> String {
    format!("{:.1}s", d.as_secs_f64())
}

/// ("bash", "cargo build") → "Bash(cargo build)"
pub fn tool_label(name: &str, title: &str) -> String {
    let mut chars = name.chars();
    let cap = match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    };
    if title.is_empty() {
        cap
    } else {
        format!("{cap}({title})")
    }
}

pub fn usage_line(usage: Option<&Usage>, steps: u32) -> String {
    match usage {
        Some(u) => {
            let cache = u
                .cache_hit_tokens
                .map(|hit| format!(" (缓存 {})", tokens(hit)))
                .unwrap_or_default();
            format!(
                "↑{}{cache} ↓{} · {steps} 步",
                tokens(u.input_tokens),
                tokens(u.output_tokens)
            )
        }
        None => format!("{steps} 步"),
    }
}

/// 把主目录前缀显示为 `~`。
pub fn tilde_path(path: &Path, home: Option<&Path>) -> String {
    if let Some(rest) = home.and_then(|h| path.strip_prefix(h).ok()) {
        if rest.as_os_str().is_empty() {
            return "~".into();
        }
        return format!("~/{}", rest.display());
    }
    path.display().to_string()
}

/// 去掉 ANSI 转义序列与控制字符（制表符换成 4 个空格），防止工具输出破坏终端。
pub fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for n in chars.by_ref() {
                    if ('@'..='~').contains(&n) {
                        break;
                    }
                }
            }
            continue;
        }
        if c == '\t' {
            out.push_str("    ");
        } else if !c.is_control() {
            out.push(c);
        }
    }
    out
}

pub fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max).collect();
    t.push('…');
    t
}

/// 按终端显示宽度截断（CJK 字符占两列），超出时以 `…` 结尾，结果不超过 `max` 列。
pub fn clip_width(s: &str, max: usize) -> String {
    if UnicodeWidthStr::width(s) <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut width = 0;
    for c in s.chars() {
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        if width + w + 1 > max {
            break;
        }
        out.push(c);
        width += w;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn token_units() {
        assert_eq!(tokens(420), "420");
        assert_eq!(tokens(1000), "1k");
        assert_eq!(tokens(12_300), "12.3k");
        assert_eq!(tokens(128_000), "128k");
        assert_eq!(tokens(1_000_000), "1M");
    }

    #[test]
    fn labels_and_durations() {
        assert_eq!(tool_label("bash", "cargo build"), "Bash(cargo build)");
        assert_eq!(tool_label("read", ""), "Read");
        assert_eq!(secs(Duration::from_millis(2300)), "2.3s");
    }

    #[test]
    fn usage_formats() {
        let u = Usage {
            input_tokens: 12_300,
            output_tokens: 420,
            cache_hit_tokens: Some(11_800),
            cache_miss_tokens: Some(500),
        };
        assert_eq!(usage_line(Some(&u), 4), "↑12.3k (缓存 11.8k) ↓420 · 4 步");
        let u = Usage {
            input_tokens: 100,
            output_tokens: 5,
            cache_hit_tokens: None,
            cache_miss_tokens: None,
        };
        assert_eq!(usage_line(Some(&u), 1), "↑100 ↓5 · 1 步");
        assert_eq!(usage_line(None, 2), "2 步");
    }

    #[test]
    fn tilde() {
        let home = PathBuf::from("/Users/me");
        assert_eq!(
            tilde_path(Path::new("/Users/me/Projects/x"), Some(&home)),
            "~/Projects/x"
        );
        assert_eq!(tilde_path(Path::new("/Users/me"), Some(&home)), "~");
        assert_eq!(tilde_path(Path::new("/tmp"), Some(&home)), "/tmp");
    }

    #[test]
    fn sanitize_strips_ansi_and_controls() {
        assert_eq!(
            sanitize("\x1b[32mCompiling\x1b[0m a\tb\r\x07"),
            "Compiling a    b"
        );
    }

    #[test]
    fn clip_by_display_width() {
        assert_eq!(clip_width("abc", 5), "abc");
        assert_eq!(clip_width("abcdef", 5), "abcd…");
        assert_eq!(clip_width("你好世界", 5), "你好…");
    }

    #[test]
    fn clip_long_lines() {
        assert_eq!(clip("你好世界", 2), "你好…");
        assert_eq!(clip("ab", 5), "ab");
    }
}
