//! 知识库：内置（官方维护、只读、可远程更新）与外置（用户或 agent 自建、可写）。
//!
//! 两个目录刻意分开，便于识别来源，也避免误改官方内容：
//! - 内置 `<数据目录>/kb`        首次使用时从二进制内嵌内容释放；`kb update` 拉取最新
//! - 外置 `<数据目录>/kb-custom` 由 `kb_add` / `kb_edit` 写入
//!
//! 内置目录受保护：`edit` 工具拒绝写入它，`bash` 由 `command_mentions_builtin` 按命令文本挡下。

use std::{
    fs,
    io::{self, Write},
    path::{Component, Path, PathBuf},
    time::Duration,
};

use regex::RegexBuilder;
use serde::{Deserialize, Serialize};

use crate::config;

use crate::embedded::EMBEDDED_FILES;

/// 远程索引与本地各存一份：它就是"最近一次应用成功的内置条目清单"。
const INDEX_FILE: &str = "index.json";
/// 知识库条目只支持 markdown。
const ENTRY_EXT: &str = "md";
/// 随机携带在二进制里的索引（首次释放内置知识库时写盘）。
const EMBEDDED_INDEX: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../kb/index.json"));
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// 远程地址默认值；可用 `[tools.kb] base_url` 或环境变量覆盖。
pub const DEFAULT_BASE_URL: &str = "https://raw.githubusercontent.com/yxxbc/Seanbot/main/kb";
pub const BASE_URL_ENV: &str = "SEANBOT_KB_BASE_URL";

#[derive(Debug, thiserror::Error)]
pub enum KbError {
    #[error("知识库读写失败：{0}")]
    Io(#[from] io::Error),
    #[error("知识库索引格式错误：{0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Config(#[from] config::ConfigError),
    #[error("{0}")]
    BadName(String),
    #[error("条目不存在：{0}")]
    NotFound(String),
    #[error("访问 {url} 失败：{reason}")]
    Http { url: String, reason: String },
}

/// 内置 / 外置 / 两者。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    All,
    Builtin,
    Custom,
}

impl Scope {
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "" | "all" => Some(Self::All),
            "builtin" => Some(Self::Builtin),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }

    /// 结果里的来源标记。
    pub fn label(self) -> &'static str {
        match self {
            Self::Builtin => "内置",
            Self::Custom => "外置",
            Self::All => "全部",
        }
    }

    fn dirs<'a>(self, builtin: &'a Path, custom: &'a Path) -> Vec<(Self, &'a Path)> {
        match self {
            Self::All => vec![(Self::Builtin, builtin), (Self::Custom, custom)],
            Self::Builtin => vec![(Self::Builtin, builtin)],
            Self::Custom => vec![(Self::Custom, custom)],
        }
    }
}

/// 知识库索引：随仓库分发，也由 `kb update` 从远程取回。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KbIndex {
    pub files: Vec<String>,
}

/// 一条知识库条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub scope: Scope,
    /// 相对条目名，例如 AboutSeanbot/01-Seanbot.md
    pub name: String,
    pub path: PathBuf,
    /// 文件里第一个一级标题，没有则用条目名
    pub title: String,
    pub bytes: u64,
}

/// `kb_search` 的一条命中。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub scope: Scope,
    pub name: String,
    pub line: usize,
    pub text: String,
}

/// 单条内置条目的更新结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Added,
    Updated,
}

impl Change {
    pub fn label(self) -> &'static str {
        match self {
            Self::Added => "新增",
            Self::Updated => "更新",
        }
    }
}

/// `kb update` 的汇总。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateReport {
    pub changed: Vec<(String, Change)>,
    pub unchanged: usize,
    pub removed: Vec<String>,
}

impl UpdateReport {
    pub fn is_up_to_date(&self) -> bool {
        self.changed.is_empty() && self.removed.is_empty()
    }

    /// 给模型或用户看的一行摘要。
    pub fn summary(&self) -> String {
        if self.is_up_to_date() {
            return format!("已是最新（{} 个条目未变）", self.unchanged);
        }
        let mut parts = Vec::new();
        if !self.changed.is_empty() {
            parts.push(format!("更新 {} 个", self.changed.len()));
        }
        if !self.removed.is_empty() {
            parts.push(format!("移除 {} 个", self.removed.len()));
        }
        if self.unchanged > 0 {
            parts.push(format!("未变 {} 个", self.unchanged));
        }
        parts.join("，")
    }
}

/// 内置知识库目录：`<数据目录>/kb`。
pub fn builtin_dir() -> Result<PathBuf, KbError> {
    Ok(config::data_dir()?.join("kb"))
}

/// 外置知识库目录：`<数据目录>/kb-custom`。
pub fn custom_dir() -> Result<PathBuf, KbError> {
    Ok(config::data_dir()?.join("kb-custom"))
}

/// 生效的远程地址：环境变量优先，其次配置，最后内置默认值。
pub fn base_url(cfg: &config::Config) -> String {
    std::env::var(BASE_URL_ENV)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .or_else(|| {
            cfg.tools
                .kb
                .base_url
                .clone()
                .filter(|v| !v.trim().is_empty())
        })
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
}

/// 首次使用时释放内嵌的内置知识库；返回本次新写出的条目名。
///
/// 只补缺失的文件，不覆盖已存在的：远程更新过的版本不能被旧的内嵌版本顶回去。
pub fn ensure_builtin(dir: &Path) -> Result<Vec<String>, KbError> {
    let mut written = Vec::new();
    fs::create_dir_all(dir)?;
    for (name, body) in EMBEDDED_FILES {
        let path = dir.join(name);
        if path.is_file() {
            continue;
        }
        write_atomic(&path, body)?;
        written.push((*name).to_string());
    }
    let index = dir.join(INDEX_FILE);
    if !index.is_file() {
        write_atomic(&index, EMBEDDED_INDEX)?;
    }
    Ok(written)
}

/// 列出条目（内置在前，各自按名字排序）。
pub fn list(builtin: &Path, custom: &Path, scope: Scope) -> Result<Vec<Entry>, KbError> {
    let mut out = Vec::new();
    for (scope, dir) in scope.dirs(builtin, custom) {
        out.extend(entries_in(scope, dir)?);
    }
    Ok(out)
}

/// 按正则搜索条目内容，最多返回 max 条。
pub fn search(
    builtin: &Path,
    custom: &Path,
    scope: Scope,
    pattern: &str,
    max: usize,
) -> Result<Vec<Hit>, KbError> {
    let regex = RegexBuilder::new(pattern)
        .case_insensitive(true)
        .build()
        .map_err(|e| KbError::BadName(format!("搜索式不是合法正则：{e}")))?;
    let mut hits = Vec::new();
    for entry in list(builtin, custom, scope)? {
        let Ok(text) = fs::read_to_string(&entry.path) else {
            continue;
        };
        for (index, line) in text.lines().enumerate() {
            if !regex.is_match(line) {
                continue;
            }
            hits.push(Hit {
                scope: entry.scope,
                name: entry.name.clone(),
                line: index + 1,
                text: line.trim().to_string(),
            });
            if hits.len() >= max {
                return Ok(hits);
            }
        }
    }
    Ok(hits)
}

/// 目录里是否已经有这个条目（名字先做语法校验，不合法就当没有）。
pub fn has_entry(dir: &Path, name: &str) -> bool {
    entry_path(dir, name)
        .map(|path| path.is_file())
        .unwrap_or(false)
}

/// 读一个外置条目（内置条目用 read 工具直接读文件即可）。
pub fn read_custom(custom: &Path, name: &str) -> Result<(PathBuf, String), KbError> {
    let path = entry_path(custom, name)?;
    let text = fs::read_to_string(&path).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            KbError::NotFound(name.to_string())
        } else {
            KbError::Io(e)
        }
    })?;
    Ok((path, text))
}

/// 写一个外置条目（overwrite 为假时拒绝覆盖已有条目）。
pub fn write_custom(
    custom: &Path,
    name: &str,
    content: &str,
    overwrite: bool,
) -> Result<PathBuf, KbError> {
    let path = entry_path(custom, name)?;
    if path.is_file() && !overwrite {
        return Err(KbError::BadName(format!(
            "外置知识库里已经有 {}；要改内容请用 kb_edit，确实要整体覆盖就加 overwrite=true",
            display_name(custom, &path)
        )));
    }
    write_atomic(&path, &ensure_trailing_newline(content))?;
    Ok(path)
}

/// 原样写一个外置条目（不做换行归一化；kb_edit 用它把替换结果落盘）。
pub fn write_entry(custom: &Path, name: &str, content: &str) -> Result<PathBuf, KbError> {
    let path = entry_path(custom, name)?;
    write_atomic(&path, content)?;
    Ok(path)
}

/// 该路径是否位于内置知识库目录内。
pub fn is_builtin_path(builtin: &Path, path: &Path) -> bool {
    crate::tool::is_within(builtin, path)
}

/// 命令文本是否在动内置知识库目录。
///
/// 和 bash 黑名单一样，这是护栏而不是沙箱：按命令文本判断，
/// 覆盖绝对路径、`~/.seanbot/kb/`、`$SEANBOT_HOME/kb/`，以及数据目录下的相对写法 `kb/`。
pub fn command_mentions_builtin(command: &str, builtin: &Path, cwd: &Path) -> bool {
    let flat = config::flatten(command);
    if flat.contains(&config::flatten(&builtin.to_string_lossy())) {
        return true;
    }
    if [".seanbot/kb/", "seanbot_home/kb/"]
        .iter()
        .any(|needle| flat.contains(needle))
    {
        return true;
    }
    flat.contains("kb/") && same_path(&cwd.join("kb"), builtin)
}

/// 从远程拉取内置知识库；返回本次的变化。
pub async fn update(builtin: &Path, base_url: &str) -> Result<UpdateReport, KbError> {
    let base = base_url.trim_end_matches('/');
    let http = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent(concat!("sean/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| KbError::Http {
            url: base.to_string(),
            reason: e.to_string(),
        })?;

    let index_url = format!("{base}/{INDEX_FILE}");
    let index_text = fetch_text(&http, &index_url).await?;
    let index: KbIndex = serde_json::from_str(&index_text)?;
    validate_index(&index)?;

    let previous = read_index(builtin);
    fs::create_dir_all(builtin)?;

    let mut report = UpdateReport::default();
    for name in &index.files {
        let url = format!("{base}/{name}");
        let remote = fetch_text(&http, &url).await?;
        let path = builtin.join(name);
        let current = fs::read_to_string(&path).ok();
        if current.as_deref() == Some(remote.as_str()) {
            report.unchanged += 1;
            continue;
        }
        let change = if current.is_some() {
            Change::Updated
        } else {
            Change::Added
        };
        write_atomic(&path, &remote)?;
        report.changed.push((name.clone(), change));
    }

    // 远程删掉的条目：只清理我们记录过的，避免误删用户放进内置目录的东西
    for name in &previous {
        if index.files.contains(name) {
            continue;
        }
        let path = builtin.join(name);
        if path.is_file() {
            fs::remove_file(&path)?;
            report.removed.push(name.clone());
        }
    }

    write_atomic(&builtin.join(INDEX_FILE), &index_text)?;
    Ok(report)
}

/// 内置知识库里有哪些条目（按本地索引记录，读不出来就当空）。
fn read_index(dir: &Path) -> Vec<String> {
    fs::read_to_string(dir.join(INDEX_FILE))
        .ok()
        .and_then(|text| serde_json::from_str::<KbIndex>(&text).ok())
        .map(|index| index.files)
        .unwrap_or_default()
}

/// 索引里的路径必须是安全的相对 markdown 路径。
fn validate_index(index: &KbIndex) -> Result<(), KbError> {
    if index.files.is_empty() {
        return Err(KbError::BadName("远程索引里没有任何条目".into()));
    }
    for name in &index.files {
        relative_entry(name).map_err(|e| KbError::BadName(format!("索引里的条目不合法：{e}")))?;
    }
    Ok(())
}

/// 列一个目录下的条目。
fn entries_in(scope: Scope, dir: &Path) -> Result<Vec<Entry>, KbError> {
    let mut out = Vec::new();
    collect_entries(scope, dir, dir, &mut out)?;
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn collect_entries(
    scope: Scope,
    root: &Path,
    dir: &Path,
    out: &mut Vec<Entry>,
) -> Result<(), KbError> {
    let listing = match fs::read_dir(dir) {
        Ok(listing) => listing,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(KbError::Io(e)),
    };
    for entry in listing {
        let path = entry?.path();
        if path.is_dir() {
            collect_entries(scope, root, &path, out)?;
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some(ENTRY_EXT) {
            continue;
        }
        let name = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let text = fs::read_to_string(&path).unwrap_or_default();
        let bytes = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        out.push(Entry {
            scope,
            title: title_of(&text).unwrap_or_else(|| name.clone()),
            name,
            path,
            bytes,
        });
    }
    Ok(())
}

/// 文件里的第一个一级标题。
fn title_of(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.trim().strip_prefix("# ").map(|t| t.trim().to_string()))
        .filter(|t| !t.is_empty())
}

/// 条目名 → 磁盘路径：先做语法校验，再确认没有越出知识库目录（符号链接也要挡）。
fn entry_path(dir: &Path, name: &str) -> Result<PathBuf, KbError> {
    let rel = relative_entry(name)?;
    let path = dir.join(&rel);
    if dir.exists() && !crate::tool::is_within(dir, &path) {
        return Err(KbError::BadName(format!("条目名越出了知识库目录：{name}")));
    }
    Ok(path)
}

/// 条目名的语法校验：必须是相对路径、不跳出目录、只支持 .md；返回补齐扩展名后的相对路径。
fn relative_entry(name: &str) -> Result<PathBuf, KbError> {
    let raw = name.trim().trim_start_matches("./");
    if raw.is_empty() {
        return Err(KbError::BadName("条目名不能为空".into()));
    }
    let mut rel = PathBuf::from(raw);
    if rel.is_absolute()
        || rel.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(KbError::BadName(format!(
            "条目名必须是知识库内的相对路径，不能是 {name}"
        )));
    }
    if rel.extension().is_none() {
        rel.set_extension(ENTRY_EXT);
    }
    if rel.extension().and_then(|e| e.to_str()) != Some(ENTRY_EXT) {
        return Err(KbError::BadName(format!(
            "只支持 .{ENTRY_EXT} 条目：{name}"
        )));
    }
    Ok(rel)
}

/// 展示用名字：相对知识库目录的路径。
fn display_name(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn ensure_trailing_newline(content: &str) -> String {
    if content.ends_with('\n') {
        content.to_string()
    } else {
        format!("{content}\n")
    }
}

async fn fetch_text(http: &reqwest::Client, url: &str) -> Result<String, KbError> {
    let response = http.get(url).send().await.map_err(|e| KbError::Http {
        url: url.to_string(),
        reason: e.to_string(),
    })?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        let head: String = body.chars().take(120).collect();
        return Err(KbError::Http {
            url: url.to_string(),
            reason: format!("HTTP {status} {head}"),
        });
    }
    Ok(body)
}

/// 先写临时文件再改名：中途失败不会留下半个条目。
fn write_atomic(path: &Path, content: &str) -> Result<(), KbError> {
    if let Some(parent) = path.parent() {
        create_dir_private(parent)?;
    }
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "entry".into());
    let tmp = path.with_file_name(format!(".{file_name}.tmp"));
    {
        let mut file = open_private(&tmp)?;
        file.write_all(content.as_bytes())?;
        file.flush()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

fn create_dir_private(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn open_private(path: &Path) -> io::Result<fs::File> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
}

/// 两个路径是否指向同一处（存在则比真实路径，否则比词法规范化）。
fn same_path(a: &Path, b: &Path) -> bool {
    fn real(path: &Path) -> PathBuf {
        fs::canonicalize(path).unwrap_or_else(|_| crate::tool::lexical_normalize(path))
    }
    real(a) == real(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path as path_match},
    };

    fn temp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    /// kb/index.json 必须与 kb/ 下实际文件一致，否则远程更新会漏文件。
    #[test]
    fn embedded_index_matches_embedded_files() {
        let index: KbIndex = serde_json::from_str(EMBEDDED_INDEX).unwrap();
        let mut declared = index.files.clone();
        declared.sort();
        let mut actual: Vec<String> = EMBEDDED_FILES
            .iter()
            .map(|(name, _)| (*name).to_string())
            .collect();
        actual.sort();
        assert_eq!(declared, actual, "kb/index.json 与 kb/ 下实际文件不一致");
    }

    #[test]
    fn ensure_builtin_releases_and_stays_idempotent() {
        let dir = temp();
        let builtin = dir.path().join("kb");
        let released = ensure_builtin(&builtin).unwrap();
        assert_eq!(released.len(), EMBEDDED_FILES.len());
        assert!(builtin.join(INDEX_FILE).is_file());

        // 本地改过的条目不能被旧的内嵌版本顶回去
        let first = builtin.join(&released[0]);
        fs::write(&first, "本地改动\n").unwrap();
        assert!(ensure_builtin(&builtin).unwrap().is_empty());
        assert_eq!(fs::read_to_string(&first).unwrap(), "本地改动\n");
    }

    #[test]
    fn list_and_search_respect_scope() {
        let dir = temp();
        let builtin = dir.path().join("kb");
        let custom = dir.path().join("kb-custom");
        ensure_builtin(&builtin).unwrap();
        write_custom(
            &custom,
            "notes/about.md",
            "# 我的笔记\nSeanbot 知识库测试\n",
            false,
        )
        .unwrap();

        let all = list(&builtin, &custom, Scope::All).unwrap();
        assert!(all.iter().any(|e| e.scope == Scope::Builtin));
        assert!(
            all.iter()
                .any(|e| e.name == "notes/about.md" && e.scope == Scope::Custom)
        );
        assert_eq!(
            list(&builtin, &custom, Scope::Builtin).unwrap().len(),
            EMBEDDED_FILES.len()
        );

        let builtin_hits = search(&builtin, &custom, Scope::Builtin, "Seanbot", 50).unwrap();
        assert!(!builtin_hits.is_empty());
        assert!(builtin_hits.iter().all(|h| h.scope == Scope::Builtin));

        let custom_hits = search(&builtin, &custom, Scope::Custom, "知识库测试", 50).unwrap();
        assert_eq!(custom_hits.len(), 1);
        assert_eq!(custom_hits[0].name, "notes/about.md");
        assert_eq!(custom_hits[0].line, 2);

        // 命中上限生效；非法正则报错
        assert_eq!(
            search(&builtin, &custom, Scope::All, "Seanbot", 1)
                .unwrap()
                .len(),
            1
        );
        assert!(search(&builtin, &custom, Scope::All, "([", 5).is_err());
    }

    #[test]
    fn entry_names_are_confined_to_the_custom_dir() {
        let dir = temp();
        let custom = dir.path().join("kb-custom");
        for bad in ["../x.md", "/tmp/x.md", "a/../../x.md", "", "notes.txt"] {
            let err = write_custom(&custom, bad, "x", false).unwrap_err();
            assert!(matches!(err, KbError::BadName(_)), "{bad} 应被拒绝：{err}");
        }
        let path = write_custom(&custom, "notes/rust", "# Rust\n", false).unwrap();
        assert!(path.ends_with("notes/rust.md"), "{}", path.display());

        let err = write_custom(&custom, "notes/rust", "改", false).unwrap_err();
        assert!(err.to_string().contains("kb_edit"), "{err}");
        write_custom(&custom, "notes/rust", "# 覆盖\n", true).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "# 覆盖\n");

        let missing = read_custom(&custom, "notes/nope.md").unwrap_err();
        assert!(matches!(missing, KbError::NotFound(_)), "{missing}");
    }

    #[test]
    fn bash_guard_matches_builtin_dir_only() {
        let dir = temp();
        let builtin = dir.path().join("kb");
        fs::create_dir_all(&builtin).unwrap();
        let cwd = dir.path();

        assert!(command_mentions_builtin(
            &format!("rm -rf {}", builtin.display()),
            &builtin,
            cwd
        ));
        assert!(command_mentions_builtin(
            "cat ~/.seanbot/kb/AboutSeanbot/01-Seanbot.md",
            &builtin,
            cwd
        ));
        assert!(command_mentions_builtin(
            "echo x > $SEANBOT_HOME/kb/a.md",
            &builtin,
            cwd
        ));
        // cwd 就是数据目录时，相对写法 kb/ 也指向内置目录
        assert!(command_mentions_builtin("rm kb/index.json", &builtin, cwd));
        // 无关命令不能误伤
        assert!(!command_mentions_builtin(
            "ls docs/ && cargo test",
            &builtin,
            cwd
        ));
        assert!(!command_mentions_builtin("grep -r kb_ src/", &builtin, cwd));
    }

    async fn mount(server: &MockServer, url_path: &str, body: &str) {
        Mock::given(method("GET"))
            .and(path_match(url_path))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn update_adds_updates_and_removes_entries() {
        let dir = temp();
        let builtin = dir.path().join("kb");

        let server = MockServer::start().await;
        mount(&server, "/index.json", r#"{"files":["a.md","b.md"]}"#).await;
        mount(&server, "/a.md", "# A\n").await;
        mount(&server, "/b.md", "# B\n").await;

        let report = update(&builtin, &server.uri()).await.unwrap();
        assert_eq!(report.changed.len(), 2);
        assert!(report.changed.iter().all(|(_, c)| *c == Change::Added));
        assert_eq!(fs::read_to_string(builtin.join("a.md")).unwrap(), "# A\n");

        let again = update(&builtin, &server.uri()).await.unwrap();
        assert!(again.is_up_to_date());
        assert_eq!(again.unchanged, 2);

        // b 改了内容、c 顶替 a：应更新 b 并清掉 a
        let server2 = MockServer::start().await;
        mount(&server2, "/index.json", r#"{"files":["b.md","c.md"]}"#).await;
        mount(&server2, "/b.md", "# B2\n").await;
        mount(&server2, "/c.md", "# C\n").await;
        let third = update(&builtin, &server2.uri()).await.unwrap();
        assert!(
            third
                .changed
                .iter()
                .any(|(name, change)| name == "b.md" && *change == Change::Updated),
            "{:?}",
            third.changed
        );
        assert_eq!(third.removed, vec!["a.md".to_string()]);
        assert!(!builtin.join("a.md").exists());
        assert!(builtin.join("c.md").is_file());
        assert_eq!(fs::read_to_string(builtin.join("b.md")).unwrap(), "# B2\n");
    }

    #[tokio::test]
    async fn update_reports_http_error_and_rejects_unsafe_index() {
        let dir = temp();
        let builtin = dir.path().join("kb");

        let missing = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&missing)
            .await;
        let err = update(&builtin, &missing.uri()).await.unwrap_err();
        assert!(err.to_string().contains("404"), "{err}");

        let unsafe_index = MockServer::start().await;
        mount(&unsafe_index, "/index.json", r#"{"files":["../evil.md"]}"#).await;
        let err = update(&builtin, &unsafe_index.uri()).await.unwrap_err();
        assert!(matches!(err, KbError::BadName(_)), "{err}");
        assert!(!dir.path().join("evil.md").exists());
    }
}
