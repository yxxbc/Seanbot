//! 项目指令文件（AGENTS.md / AGENT.md / CLAUDE.md）：发现、读取与大小限制。
//!
//! 这些文件在**会话开始时读一次**并注入系统提示词，因此会话内保持不变（前缀缓存依赖于此）。
//! 查找顺序：全局 `<数据目录>/AGENTS.md` 等，然后从工作目录逐级向上到文件系统根，
//! 每层取第一个命中的文件名；全局在前、越具体越靠后，冲突时以后者为准。

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// 一层目录里认的文件名，按优先级。
pub const FILE_NAMES: [&str; 3] = ["AGENTS.md", "AGENT.md", "CLAUDE.md"];
/// 所有指令文件合计最多注入的字节数，超出的部分截断。
pub const MAX_TOTAL_BYTES: usize = 48 * 1024;

/// 指令文件来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// 数据目录下的全局指令。
    Global,
    /// 工作目录链上的项目指令。
    Project,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Self::Global => "全局",
            Self::Project => "项目",
        }
    }
}

/// 一个要被注入的指令文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionFile {
    pub scope: Scope,
    pub path: PathBuf,
    pub content: String,
    /// 因为总大小限制被截断了
    pub truncated: bool,
}

/// 发现并读取指令文件：全局在前，工作目录链上的从远到近。
pub fn discover(cwd: &Path, data_dir: Option<&Path>) -> Vec<InstructionFile> {
    let mut candidates: Vec<(Scope, PathBuf)> = Vec::new();

    if let Some(dir) = data_dir
        && let Some(path) = first_in(dir)
    {
        candidates.push((Scope::Global, path));
    }

    // 从工作目录往上到根，先收集再反转，保证"越具体越靠后"
    let mut chain: Vec<PathBuf> = Vec::new();
    let mut current = Some(cwd.to_path_buf());
    while let Some(dir) = current {
        chain.push(dir.clone());
        current = dir.parent().map(Path::to_path_buf);
    }
    for level in chain.into_iter().rev() {
        if let Some(path) = first_in(&level) {
            candidates.push((Scope::Project, path));
        }
    }

    // 同一个文件可能出现两次（例如主目录既在链上、又是数据目录），只算一次
    let mut seen = HashSet::new();
    let mut budget = MAX_TOTAL_BYTES;
    let mut out = Vec::new();
    for (scope, path) in candidates {
        let key = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if !seen.insert(key) {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        if text.trim().is_empty() {
            continue;
        }
        let (content, truncated) = take_within(&text, budget);
        if content.is_empty() {
            continue;
        }
        budget = budget.saturating_sub(content.len());
        out.push(InstructionFile {
            scope,
            path,
            content: content.to_string(),
            truncated,
        });
    }
    out
}

/// 一层目录里优先级最高的那个文件。
fn first_in(dir: &Path) -> Option<PathBuf> {
    FILE_NAMES
        .iter()
        .map(|name| dir.join(name))
        .find(|path| path.is_file())
}

/// 在预算内按字符边界截取，返回（片段，是否截断）。
fn take_within(text: &str, budget: usize) -> (&str, bool) {
    if text.len() <= budget {
        return (text, false);
    }
    let mut end = budget.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

/// 本会话已经注入过哪些目录的指令文件（同一个文件只注入一次，别反复塞上下文）。
#[derive(Debug, Clone, Default)]
pub struct InjectedInstructions {
    inner: Arc<Mutex<HashSet<PathBuf>>>,
}

impl InjectedInstructions {
    /// 取"这个文件所在目录（到工作目录之间）里尚未注入过"的指令文件，从远到近。
    ///
    /// 工作目录自己的那份已经在系统提示词里了，这里不重复；工作目录之外的路径直接跳过。
    pub fn take_for(&self, file: &Path, cwd: &Path) -> Vec<InstructionFile> {
        let mut dirs = Vec::new();
        let mut current = file.parent().map(Path::to_path_buf);
        while let Some(dir) = current {
            if dir == cwd || !crate::tool::is_within(cwd, &dir) {
                break;
            }
            dirs.push(dir.clone());
            current = dir.parent().map(Path::to_path_buf);
        }
        dirs.reverse();

        let mut taken = Vec::new();
        let mut seen = self.inner.lock().unwrap();
        for dir in dirs {
            let Some(path) = first_in(&dir) else {
                continue;
            };
            let key = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            if !seen.insert(key) {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            if text.trim().is_empty() {
                continue;
            }
            let (content, truncated) = take_within(&text, MAX_TOTAL_BYTES);
            taken.push(InstructionFile {
                scope: Scope::Project,
                path,
                content: content.to_string(),
                truncated,
            });
        }
        taken
    }

    /// 换会话时清空（@BT@/new@BT@、@BT@/resume@BT@、@BT@/clear@BT@）。
    pub fn clear(&self) {
        self.inner.lock().unwrap().clear();
    }
}

/// 把刚注入的目录指令渲染成一段工具结果文本。
pub fn render_block(files: &[InstructionFile]) -> Option<String> {
    if files.is_empty() {
        return None;
    }
    let mut out = String::from(
        "\n\n# 目录指令（随本次访问注入）\n以下文件来自你刚访问的目录，属于项目约定，优先照做；与安全护栏冲突时以护栏为准。\n",
    );
    for file in files {
        out.push_str(&format!(
            "\n## {}\n{}\n",
            file.path.display(),
            file.content.trim_end()
        ));
        if file.truncated {
            out.push_str("（内容过长，已截断）\n");
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    #[test]
    fn nested_files_are_taken_once_from_far_to_near() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("proj");
        let inner = root.join("crates/app");
        write(&root.join("crates/AGENTS.md"), "crates 层约定\n");
        write(&inner.join("AGENTS.md"), "app 层约定\n");

        let tracker = InjectedInstructions::default();
        let taken = tracker.take_for(&inner.join("src/main.rs"), &root);
        let names: Vec<String> = taken.iter().map(|f| f.path.display().to_string()).collect();
        assert_eq!(names.len(), 2, "{names:?}");
        // 用路径比较而不是字符串包含：Windows 的分隔符是 `\`
        let expected = |parts: &[&str]| parts.iter().fold(PathBuf::new(), |p, s| p.join(s));
        assert!(
            taken[0].path.ends_with(expected(&["crates", "AGENTS.md"])),
            "远的先来：{names:?}"
        );
        assert!(
            taken[1].path.ends_with(expected(&["app", "AGENTS.md"])),
            "{names:?}"
        );

        // 同一个文件只注入一次，哪怕换个文件再读
        assert!(
            tracker
                .take_for(&inner.join("src/other.rs"), &root)
                .is_empty()
        );
        // 工作目录自己的那份在系统提示词里，不在按需注入范围
        assert!(tracker.take_for(&root.join("x.rs"), &root).is_empty());
    }

    #[test]
    fn render_block_needs_files_and_shows_paths() {
        assert!(render_block(&[]).is_none());
        let block = render_block(&[InstructionFile {
            scope: Scope::Project,
            path: PathBuf::from("/p/crates/AGENTS.md"),
            content: "只改这个 crate\n".into(),
            truncated: false,
        }])
        .unwrap();
        assert!(block.contains("# 目录指令"), "{block}");
        assert!(block.contains("/p/crates/AGENTS.md"), "{block}");
        assert!(block.contains("只改这个 crate"), "{block}");
        assert!(block.contains("护栏"), "要写明不能覆盖安全规则：{block}");
    }

    #[test]
    fn picks_up_project_files_and_orders_them_specific_last() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("proj");
        let nested = root.join("crates/app");
        write(&root.join("AGENTS.md"), "根目录指令\n");
        write(&nested.join("CLAUDE.md"), "子目录指令\n");

        let found = discover(&nested, None);
        let names: Vec<String> = found
            .iter()
            .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"AGENTS.md".to_string()), "{names:?}");
        assert!(names.contains(&"CLAUDE.md".to_string()), "{names:?}");
        // 越靠近工作目录越靠后
        let agents = found
            .iter()
            .position(|f| f.path.ends_with("AGENTS.md"))
            .unwrap();
        let claude = found
            .iter()
            .position(|f| f.path.ends_with("CLAUDE.md"))
            .unwrap();
        assert!(agents < claude, "顺序应为 根 → 子：{names:?}");
    }

    #[test]
    fn one_file_per_directory_with_fixed_priority() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("proj");
        write(&root.join("AGENTS.md"), "一\n");
        write(&root.join("AGENT.md"), "二\n");
        write(&root.join("CLAUDE.md"), "三\n");
        let found = discover(&root, None);
        let ours: Vec<&InstructionFile> =
            found.iter().filter(|f| f.path.starts_with(&root)).collect();
        assert_eq!(ours.len(), 1, "一层只取一个：{:?}", ours);
        assert!(ours[0].path.ends_with("AGENTS.md"));
    }

    #[test]
    fn global_file_is_first_and_duplicates_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        write(&data.join("AGENTS.md"), "全局指令\n");
        let project = dir.path().join("proj");
        write(&project.join("AGENTS.md"), "项目指令\n");

        let found = discover(&project, Some(&data));
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].scope, Scope::Global);
        assert!(found[0].content.contains("全局指令"));
        assert_eq!(found[1].scope, Scope::Project);

        // 数据目录就在链上时，同一个文件不能重复注入
        let same = discover(&data, Some(&data));
        assert_eq!(same.iter().filter(|f| f.scope == Scope::Global).count(), 1);
    }

    #[test]
    fn oversized_files_are_truncated_on_char_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("proj");
        write(&root.join("AGENTS.md"), &"多".repeat(MAX_TOTAL_BYTES));
        let found = discover(&root, None);
        let ours: Vec<&InstructionFile> =
            found.iter().filter(|f| f.path.starts_with(&root)).collect();
        assert_eq!(ours.len(), 1);
        assert!(ours[0].truncated);
        assert!(ours[0].content.len() <= MAX_TOTAL_BYTES);
        assert!(ours[0].content.chars().all(|c| c == '多'), "不能截出半个字");
    }

    #[test]
    fn missing_or_empty_files_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("proj");
        fs::create_dir_all(&root).unwrap();
        write(&root.join("CLAUDE.md"), "   \n\n");
        let found = discover(&root, None);
        assert!(
            found.iter().all(|f| !f.path.starts_with(&root)),
            "{found:?}"
        );
    }
}
