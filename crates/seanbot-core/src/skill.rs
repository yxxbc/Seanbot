//! 技能：一个目录里的 `SKILL.md`（或单个 `.md` 文件），frontmatter 里带 name / description。
//!
//! 两处来源：
//! - 项目：`<工作目录>/.seanbot/skills/`（边找边向上，越近优先级越高）
//! - 全局：`<数据目录>/skills/`
//!
//! 系统提示词里只列 name + description（省上下文），正文由 `skill` 工具按需加载——
//! 这是这套机制的要点：知道有什么技能，但只在真正要做那件事时才读全文。

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

/// 目录形式的技能入口文件名。
pub const SKILL_FILE: &str = "SKILL.md";
/// 项目技能相对工作目录的位置。
pub const PROJECT_SUBDIR: &str = ".seanbot/skills";
/// 全局技能相对数据目录的位置。
pub const GLOBAL_SUBDIR: &str = "skills";
/// 单次加载技能正文的字节上限，超出截断。
pub const MAX_SKILL_BYTES: usize = 64 * 1024;
/// 提示词里 description 的字符上限。
const MAX_DESCRIPTION_CHARS: usize = 160;
/// 缺少 description 时，用正文首行的截断长度。
const FALLBACK_DESCRIPTION_CHARS: usize = 120;
/// 列技能资源文件的上限。
const MAX_SIBLINGS: usize = 20;

/// 技能来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Global,
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

/// 一个可用技能。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub scope: Scope,
    pub name: String,
    pub description: String,
    /// 技能目录（放参考文件、脚本的地方）
    pub dir: PathBuf,
    /// 入口文件（SKILL.md 或单个 .md）
    pub path: PathBuf,
    pub bytes: u64,
}

impl Skill {
    /// 目录形式的技能才有资源目录可列。
    pub fn has_resource_dir(&self) -> bool {
        self.path.file_name().and_then(|n| n.to_str()) == Some(SKILL_FILE)
    }
}

/// 发现结果：技能清单 + 解析时的问题。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Discovery {
    pub skills: Vec<Skill>,
    pub warnings: Vec<String>,
}

/// 加载出来的技能正文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedSkill {
    pub content: String,
    pub truncated: bool,
    /// 技能目录下的其它文件（相对路径），供继续用 read 打开
    pub siblings: Vec<String>,
}

/// 发现技能：项目（越近越优先）在前，然后是全局；同名只保留优先级最高的那个。
pub fn discover(cwd: &Path, data_dir: Option<&Path>) -> Discovery {
    let mut out = Discovery::default();
    let mut seen = HashSet::new();

    let mut roots: Vec<(Scope, PathBuf)> = Vec::new();
    let mut current = Some(cwd.to_path_buf());
    while let Some(dir) = current {
        roots.push((Scope::Project, dir.join(PROJECT_SUBDIR)));
        current = dir.parent().map(Path::to_path_buf);
    }
    if let Some(dir) = data_dir {
        roots.push((Scope::Global, dir.join(GLOBAL_SUBDIR)));
    }

    let mut claimed = HashSet::new();
    for (scope, root) in roots {
        // 同一个目录可能既是项目链上的一层、又是全局目录
        let key = fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
        if !claimed.insert(key) {
            continue;
        }
        for skill in skills_in(scope, &root, &mut out.warnings) {
            if !seen.insert(skill.name.clone()) {
                continue;
            }
            out.skills.push(skill);
        }
    }

    // 项目在前、同来源按名字排序，读起来稳定
    out.skills.sort_by(|a, b| {
        let scope = |s: Scope| matches!(s, Scope::Project) as u8;
        scope(b.scope)
            .cmp(&scope(a.scope))
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

/// 按名字找一个技能。
pub fn find<'a>(skills: &'a [Skill], name: &str) -> Option<&'a Skill> {
    let wanted = name.trim();
    skills.iter().find(|s| s.name == wanted)
}

/// 读取技能正文；目录形式还会列出同目录下的其它文件。
pub fn load(skill: &Skill) -> std::io::Result<LoadedSkill> {
    let text = fs::read_to_string(&skill.path)?;
    let (content, truncated) = take_within(&text, MAX_SKILL_BYTES);
    let siblings = if skill.has_resource_dir() {
        siblings(&skill.dir)
    } else {
        Vec::new()
    };
    Ok(LoadedSkill {
        content: content.to_string(),
        truncated,
        siblings,
    })
}

/// 列一个技能根目录下的技能（目录形式与单文件形式都认）。
fn skills_in(scope: Scope, root: &Path, warnings: &mut Vec<String>) -> Vec<Skill> {
    let Ok(listing) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in listing.flatten() {
        let path = entry.path();
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if file_name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            let entry_file = path.join(SKILL_FILE);
            if entry_file.is_file() {
                match read_skill(scope, &path, &entry_file, warnings) {
                    Some(skill) => out.push(skill),
                    None => continue,
                }
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            let dir = path.parent().unwrap_or(root).to_path_buf();
            if let Some(skill) = read_skill(scope, &dir, &path, warnings) {
                out.push(skill);
            }
        }
    }
    out
}

/// 解析一个技能：frontmatter 里的 name / description，缺了就用目录名与正文首行兜底。
fn read_skill(scope: Scope, dir: &Path, path: &Path, warnings: &mut Vec<String>) -> Option<Skill> {
    let text = fs::read_to_string(path).ok()?;
    if text.trim().is_empty() {
        warnings.push(format!("{} 是空文件，已跳过", path.display()));
        return None;
    }
    let (fields, body) = split_frontmatter(&text);
    let stem = if path.file_name().and_then(|n| n.to_str()) == Some(SKILL_FILE) {
        dir.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        path.file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };

    let name = fields
        .iter()
        .find(|(key, _)| key == "name")
        .map(|(_, value)| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            if !stem.is_empty() {
                warnings.push(format!(
                    "{} 的 frontmatter 缺少 name，已用「{stem}」代替",
                    path.display()
                ));
            }
            stem.clone()
        });
    if name.is_empty() {
        warnings.push(format!(
            "{} 既没有 name 也推不出名字，已跳过",
            path.display()
        ));
        return None;
    }
    if !stem.is_empty() && name != stem {
        warnings.push(format!(
            "{} 的 frontmatter name（{name}）与目录名（{stem}）不一致，按 frontmatter 处理",
            path.display()
        ));
    }

    let description = fields
        .iter()
        .find(|(key, _)| key == "description")
        .map(|(_, value)| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            warnings.push(format!(
                "{} 的 frontmatter 缺少 description，已用正文首行代替",
                path.display()
            ));
            clip(
                &first_line(body).unwrap_or_default(),
                FALLBACK_DESCRIPTION_CHARS,
            )
        });

    let bytes = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    Some(Skill {
        scope,
        name,
        description: clip(&description, MAX_DESCRIPTION_CHARS),
        dir: dir.to_path_buf(),
        path: path.to_path_buf(),
        bytes,
    })
}

/// 分离 frontmatter（`---` 包起来的一小段 key: value）与正文。
fn split_frontmatter(text: &str) -> (Vec<(String, String)>, &str) {
    let mut lines = text.lines();
    let Some(first) = lines.next() else {
        return (Vec::new(), "");
    };
    if first.trim() != "---" {
        return (Vec::new(), text);
    }
    let mut fields = Vec::new();
    let mut consumed = first.len() + 1;
    for line in lines {
        consumed += line.len() + 1;
        if line.trim() == "---" {
            let body = text.get(consumed..).unwrap_or("");
            return (fields, body);
        }
        if let Some((key, value)) = line.split_once(':') {
            let value = value.trim().trim_matches(['"', '\'']).to_string();
            fields.push((key.trim().to_ascii_lowercase(), value));
        }
    }
    // 没有收尾的 ---：当作没有 frontmatter
    (Vec::new(), text)
}

/// 正文里第一行有内容的普通文本（跳过标题与引用）。
fn first_line(body: &str) -> Option<String> {
    body.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with('>'))
        .map(|line| line.trim_start_matches(['-', '*', ' ']).trim().to_string())
        .filter(|line| !line.is_empty())
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

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

/// 技能目录下的其它文件（相对路径，跳过隐藏项与入口文件）。
fn siblings(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    collect_siblings(dir, dir, &mut out);
    out.sort();
    out.truncate(MAX_SIBLINGS);
    out
}

fn collect_siblings(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(listing) = fs::read_dir(dir) else {
        return;
    };
    for entry in listing.flatten() {
        let path = entry.path();
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if file_name.starts_with('.') {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if path.is_dir() {
            collect_siblings(root, &path, out);
        } else if rel != SKILL_FILE {
            out.push(rel);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn skill_file(root: &Path, name: &str, front: &str, body: &str) -> PathBuf {
        let path = root.join(name).join(SKILL_FILE);
        write(&path, &format!("---\n{front}\n---\n{body}"));
        path
    }

    #[test]
    fn finds_skills_in_both_shapes_and_sorts_project_first() {
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("proj");
        let roots = proj.join(PROJECT_SUBDIR);
        skill_file(
            &roots,
            "fix-imports",
            "name: fix-imports\ndescription: 修导入",
            "# 步骤\n先看再改\n",
        );
        write(
            &roots.join("quick.md"),
            "---\nname: quick\ndescription: 单文件技能\n---\n正文\n",
        );
        // 没有 frontmatter：用目录名 + 正文首行兜底
        skill_file(&roots, "legacy", "", "# 老技能\n第一行说明\n");

        let data = dir.path().join("data");
        skill_file(
            &data.join(GLOBAL_SUBDIR),
            "deploy",
            "name: deploy\ndescription: 发布流程\n",
            "正文\n",
        );

        let found = discover(&proj, Some(data.as_path()));
        let names: Vec<&str> = found.skills.iter().map(|s| s.name.as_str()).collect();
        for expected in ["fix-imports", "quick", "legacy", "deploy"] {
            assert!(names.contains(&expected), "{expected} 没被发现：{names:?}");
        }
        assert_eq!(found.skills[0].scope, Scope::Project);
        assert_eq!(found.skills.last().unwrap().scope, Scope::Global);

        let legacy = find(&found.skills, "legacy").unwrap();
        assert_eq!(legacy.description, "第一行说明");
        assert!(
            found.warnings.iter().any(|w| w.contains("legacy")),
            "{:?}",
            found.warnings
        );
    }

    #[test]
    fn project_skill_shadows_global_with_same_name() {
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("proj");
        let data = dir.path().join("data");
        skill_file(
            &proj.join(PROJECT_SUBDIR),
            "same",
            "name: same\ndescription: 项目的\n",
            "项目正文\n",
        );
        skill_file(
            &data.join(GLOBAL_SUBDIR),
            "same",
            "name: same\ndescription: 全局的\n",
            "全局正文\n",
        );

        let found = discover(&proj, Some(&data));
        let same: Vec<&Skill> = found.skills.iter().filter(|s| s.name == "same").collect();
        assert_eq!(same.len(), 1);
        assert_eq!(same[0].scope, Scope::Project);
        assert_eq!(same[0].description, "项目的");
    }

    #[test]
    fn load_returns_body_and_resource_files() {
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("proj");
        let path = skill_file(
            &proj.join(PROJECT_SUBDIR),
            "deep",
            "name: deep\ndescription: 带附件\n",
            "# 正文\n",
        );
        let skill_dir = path.parent().unwrap();
        write(&skill_dir.join("references/api.md"), "参考\n");
        write(&skill_dir.join("scripts/run.sh"), "#!/bin/sh\n");

        let skills = discover(&proj, None).skills;
        let skill = find(&skills, "deep").expect("应能找到 deep");
        let loaded = load(skill).unwrap();
        assert!(loaded.content.contains("# 正文"), "{}", loaded.content);
        assert!(!loaded.truncated);
        assert_eq!(
            loaded.siblings,
            vec![
                "references/api.md".to_string(),
                "scripts/run.sh".to_string()
            ]
        );
        assert!(find(&skills, "nope").is_none());
    }

    #[test]
    fn oversized_body_is_truncated_on_char_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("proj");
        let body = "多".repeat(MAX_SKILL_BYTES);
        skill_file(
            &proj.join(PROJECT_SUBDIR),
            "big",
            "name: big\ndescription: 很大\n",
            &body,
        );

        let skills = discover(&proj, None).skills;
        let loaded = load(find(&skills, "big").unwrap()).unwrap();
        assert!(loaded.truncated);
        assert!(loaded.content.len() <= MAX_SKILL_BYTES);
        assert!(loaded.content.ends_with('多'), "不能截出半个字");
    }

    #[test]
    fn empty_skill_file_is_skipped_with_warning() {
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("proj");
        write(
            &proj.join(PROJECT_SUBDIR).join("blank").join(SKILL_FILE),
            "   \n",
        );
        let found = discover(&proj, None);
        assert!(found.skills.is_empty(), "{:?}", found.skills);
        assert!(
            found.warnings.iter().any(|w| w.contains("空文件")),
            "{:?}",
            found.warnings
        );
    }
}
