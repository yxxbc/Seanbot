//! 斜杠命令表与浮窗过滤（设计书 §4.7）。
//!
//! 只做纯逻辑：命令表、过滤排序、匹配位置（用于高亮）。浮窗的绘制与按键在 `tui/mod.rs`。

/// 一条斜杠命令。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Command {
    pub name: &'static str,
    pub description: &'static str,
}

/// 所有命令，顺序即浮窗里的默认顺序。
pub const COMMANDS: &[Command] = &[
    Command {
        name: "/help",
        description: "显示帮助",
    },
    Command {
        name: "/new",
        description: "开始新会话（换一个会话文件）",
    },
    Command {
        name: "/resume",
        description: "从列表中选择并恢复历史会话",
    },
    Command {
        name: "/clear",
        description: "清空对话（仍在本会话）",
    },
    Command {
        name: "/model",
        description: "切换模型",
    },
    Command {
        name: "/yolo",
        description: "切换确认模式 / YOLO",
    },
    Command {
        name: "/mouse",
        description: "开关鼠标支持",
    },
    Command {
        name: "/exit",
        description: "退出",
    },
];

/// 浮窗最多显示几条。
pub const MAX_ITEMS: usize = 8;

/// 一次匹配：命令在 `COMMANDS` 里的下标 + 命中的字符位置（用于高亮）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub index: usize,
    /// 命令名里命中的字符下标（按字符，不是字节）
    pub positions: Vec<usize>,
}

/// 输入框内容是否该弹浮窗：以 `/` 开头且还没有空格。
pub fn popup_query(input: &str) -> Option<&str> {
    let trimmed = input.trim_start();
    if !trimmed.starts_with('/') || trimmed.contains(char::is_whitespace) {
        return None;
    }
    Some(trimmed)
}

/// 过滤命令：前缀匹配优先，其次子序列模糊匹配；同档按名字排序。
pub fn filter(query: &str) -> Vec<Match> {
    let query: Vec<char> = query.chars().collect();
    let mut matched: Vec<(u8, usize, Match)> = Vec::new();
    for (index, command) in COMMANDS.iter().enumerate() {
        let name: Vec<char> = command.name.chars().collect();
        if let Some(positions) = prefix_match(&name, &query) {
            matched.push((0, index, Match { index, positions }));
            continue;
        }
        if let Some(positions) = subsequence_match(&name, &query) {
            matched.push((1, index, Match { index, positions }));
        }
    }
    matched.sort_by_key(|(tier, index, _)| (*tier, *index));
    matched
        .into_iter()
        .take(MAX_ITEMS)
        .map(|(_, _, m)| m)
        .collect()
}

/// 前缀匹配：命中的位置就是前 n 个字符。
fn prefix_match(name: &[char], query: &[char]) -> Option<Vec<usize>> {
    if query.is_empty() || name.len() < query.len() {
        return None;
    }
    if name[..query.len()] == *query {
        Some((0..query.len()).collect())
    } else {
        None
    }
}

/// 子序列匹配：按顺序找到每一个字符，返回它们的位置（找不到就 None）。
fn subsequence_match(name: &[char], query: &[char]) -> Option<Vec<usize>> {
    if query.is_empty() {
        return None;
    }
    let mut positions = Vec::with_capacity(query.len());
    let mut cursor = 0;
    for wanted in query {
        let found = name[cursor..].iter().position(|c| c == wanted)? + cursor;
        positions.push(found);
        cursor = found + 1;
    }
    Some(positions)
}

/// 把命令名切成 `(文本, 是否命中)` 片段，交给调用方上色。
pub fn highlight(name: &str, positions: &[usize]) -> Vec<(String, bool)> {
    let mut pieces: Vec<(String, bool)> = Vec::new();
    for (index, c) in name.chars().enumerate() {
        let hit = positions.contains(&index);
        match pieces.last_mut() {
            Some((text, last_hit)) if *last_hit == hit => text.push(c),
            _ => pieces.push((c.to_string(), hit)),
        }
    }
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(matches: &[Match]) -> Vec<&'static str> {
        matches.iter().map(|m| COMMANDS[m.index].name).collect()
    }

    #[test]
    fn popup_only_for_a_bare_slash_prefix() {
        assert_eq!(popup_query("/"), Some("/"));
        assert_eq!(popup_query("  /mo"), Some("/mo"));
        assert_eq!(popup_query("/model x"), None, "有空格就不再是命令名");
        assert_eq!(popup_query("你好"), None);
        assert_eq!(popup_query(""), None);
    }

    #[test]
    fn prefix_matches_come_first() {
        let found = filter("/mo");
        assert_eq!(names(&found), vec!["/model", "/mouse"]);
        assert_eq!(found[0].positions, vec![0, 1, 2], "前三个字符都命中");
    }

    #[test]
    fn subsequence_matching_finds_skipped_characters() {
        let found = filter("/r");
        let listed = names(&found);
        assert!(listed.contains(&"/resume"), "{listed:?}");
        let resume = found
            .iter()
            .find(|m| COMMANDS[m.index].name == "/resume")
            .unwrap();
        assert_eq!(resume.positions, vec![0, 1], "斜杠 + r");
        assert!(filter("/zzz").is_empty());
    }

    #[test]
    fn highlight_splits_into_matched_and_plain_pieces() {
        let pieces = highlight("/model", &[0, 2, 3]);
        assert_eq!(
            pieces,
            vec![
                ("/".to_string(), true),
                ("m".to_string(), false),
                ("od".to_string(), true),
                ("el".to_string(), false),
            ]
        );
    }

    #[test]
    fn items_are_capped() {
        assert!(filter("/").len() <= MAX_ITEMS);
    }
}
