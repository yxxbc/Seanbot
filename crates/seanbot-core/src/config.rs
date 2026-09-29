//! `~/.seanbot/config.toml` 的读写。

use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, PoisonError, RwLock},
};

use serde::{Deserialize, Serialize};

pub const DEFAULT_DENY: &[&str] = &[
    "rm",
    "sudo",
    "mkfs",
    "dd",
    "shutdown",
    "reboot",
    "chmod -R",
    "git push --force",
    "git push -f",
    "Remove-Item",
    "del",
    "rd",
    "format",
];

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("无法确定用户主目录")]
    NoHome,
    #[error("读写配置失败：{0}")]
    Io(#[from] io::Error),
    #[error("配置文件格式错误：{0}")]
    Parse(#[from] toml::de::Error),
    #[error("配置序列化失败：{0}")]
    Serialize(#[from] toml::ser::Error),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub provider: String,
    pub model: String,
    pub providers: BTreeMap<String, ProviderConfig>,
    pub agent: AgentConfig,
    pub tools: ToolsConfig,
    pub ui: UiConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            provider: "deepseek".into(),
            model: "deepseek-flash".into(),
            providers: BTreeMap::new(),
            agent: AgentConfig::default(),
            tools: ToolsConfig::default(),
            ui: UiConfig::default(),
        }
    }
}

/// Agent 主循环的上限。改动由 `config` 工具写入后立即生效。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    /// 单轮最多步数（一步 = 一次模型调用）
    pub max_steps: u32,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_steps: crate::DEFAULT_MAX_STEPS,
        }
    }
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

impl std::fmt::Debug for ProviderConfig {
    /// 手写 `Debug` 而非 derive：`api_key` 打码，避免 `{:?}` / 日志漏出明文密钥。
    /// `Config` 仍为 derive，其 `Debug` 会转发到这里的实现，因此整体也是打码的。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderConfig")
            .field("api_key", &self.api_key.as_deref().map(mask_key))
            .finish()
    }
}

/// 密钥打码：保留前 4 个字符便于辨认，其余以 `…` 代替；长度不足 5 则整体隐藏。
fn mask_key(key: &str) -> String {
    const VISIBLE: usize = 4;
    let mut chars = key.chars();
    let head: String = chars.by_ref().take(VISIBLE).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        "****".to_string()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolsConfig {
    pub bash: BashConfig,
    pub read: ReadConfig,
    pub search: SearchConfig,
    pub web: WebConfig,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WebConfig {
    /// 联网服务，目前只支持 `anysearch`
    pub provider: String,
    /// 可选；环境变量 `ANYSEARCH_API_KEY` 优先；都为空时匿名访问
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// 可选；覆盖服务地址（测试或自建代理用），环境变量 `ANYSEARCH_API_BASE_URL` 优先
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// web_search 返回条数上限（模型传的 max_results 会收敛到此值）
    pub max_results: u64,
    /// web_fetch 单页最多保留的字符数
    pub page_chars: usize,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            provider: "anysearch".into(),
            api_key: None,
            base_url: None,
            max_results: 10,
            page_chars: 30_000,
        }
    }
}

impl std::fmt::Debug for WebConfig {
    /// 与 `ProviderConfig` 一致：`api_key` 打码。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebConfig")
            .field("provider", &self.provider)
            .field("api_key", &self.api_key.as_deref().map(mask_key))
            .field("base_url", &self.base_url)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BashConfig {
    /// 命令黑名单：命中即拒绝。只能由用户手动修改（模型不能通过 `config` 工具改），改动需重启生效
    pub deny: Vec<String>,
    /// 模型未指定 timeout 时的默认超时（秒）
    pub default_timeout: u64,
    /// 模型可请求的最大超时（秒）
    pub max_timeout: u64,
    /// 输出超过该字符数时只保留首尾各一半
    pub max_output: usize,
}

impl Default for BashConfig {
    fn default() -> Self {
        Self {
            deny: DEFAULT_DENY.iter().map(|s| s.to_string()).collect(),
            default_timeout: 120,
            max_timeout: 600,
            max_output: 30_000,
        }
    }
}

/// `read` 的默认读取量。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReadConfig {
    /// 未指定 limit 时最多读取的行数
    pub default_lines: u64,
}

impl Default for ReadConfig {
    fn default() -> Self {
        Self {
            default_lines: 2000,
        }
    }
}

/// `search` 的默认返回量。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchConfig {
    /// 未指定 max_results 时最多返回的条数
    pub default_results: u64,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            default_results: 200,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub show_reasoning: bool,
}

/// 数据目录：`SEANBOT_HOME` 优先，否则 `~/.seanbot`。
pub fn data_dir() -> Result<PathBuf, ConfigError> {
    if let Some(p) = std::env::var_os("SEANBOT_HOME") {
        return Ok(PathBuf::from(p));
    }
    dirs::home_dir()
        .map(|h| h.join(".seanbot"))
        .ok_or(ConfigError::NoHome)
}

pub fn config_path() -> Result<PathBuf, ConfigError> {
    Ok(data_dir()?.join("config.toml"))
}

pub fn history_path() -> Result<PathBuf, ConfigError> {
    Ok(data_dir()?.join("history"))
}

/// `path` 是否就是配置文件（按真实路径比较，兼容符号链接与尚不存在的文件）。
pub fn is_config_file(path: &Path, config_path: &Path) -> bool {
    real_path(path) == real_path(config_path)
}

/// 真实路径：存在就用 canonicalize，不存在则退回词法规范化（文件还没建时也能比对）。
fn real_path(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| crate::tool::lexical_normalize(path))
}

/// 命令文本是否提到受保护的配置文件。
///
/// 判定按命令文本做（和黑名单一样属于护栏，不是沙箱）：覆盖绝对路径、
/// `~`/`$HOME`/`%USERPROFILE%` 展开后的 `.seanbot/config.toml`、`$SEANBOT_HOME/config.toml`；
/// 工作目录就是数据目录时，相对写法 `config.toml` 也算。
pub fn command_mentions_config(command: &str, config_path: &Path, cwd: &Path) -> bool {
    let flat = flatten(command);
    if flat.contains(&flatten(&config_path.to_string_lossy())) {
        return true;
    }
    if [".seanbot/config.toml", "seanbot_home/config.toml"]
        .iter()
        .any(|needle| flat.contains(needle))
    {
        return true;
    }
    flat.contains("config.toml") && is_config_file(&cwd.join("config.toml"), config_path)
}

/// 命中保护规则时的错误文案。
pub fn bash_config_guard(command: &str, config_path: &Path, cwd: &Path) -> Result<(), String> {
    if command_mentions_config(command, config_path, cwd) {
        return Err(
            "config.toml 是受保护的配置文件，bash 不能读写它；需要改配置请用 config 工具（或手动编辑配置文件）"
                .into(),
        );
    }
    Ok(())
}

/// 命令文本归一化：小写、`\` 换成 `/`、去掉 shell 变量的大括号（`${X}` 与 `$X` 等价）。
fn flatten(text: &str) -> String {
    text.to_lowercase()
        .replace('\\', "/")
        .chars()
        .filter(|c| *c != '{' && *c != '}')
        .collect()
}

/// 写回配置文件文本：目录自动创建，文件权限 0600（与 `save_to` 一致）。
pub fn write_config_text(path: &Path, text: &str) -> Result<(), ConfigError> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(path)?;
    file.write_all(text.as_bytes())?;
    // 已存在的文件不受 mode() 影响，显式收紧权限
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// 共享的运行时配置。
///
/// 工具每次调用都读当前值，Agent 每轮步数上限也取自这里，因此 `config` 工具
/// 写盘后同步内存即可让改动立即生效，无需重启。
#[derive(Clone, Default)]
pub struct SharedConfig(Arc<RwLock<Config>>);

impl SharedConfig {
    pub fn new(config: Config) -> Self {
        Self(Arc::new(RwLock::new(config)))
    }

    /// 只读访问；`f` 内不要 await、不要做耗时操作。
    pub fn read<T>(&self, f: impl FnOnce(&Config) -> T) -> T {
        let config = self.0.read().unwrap_or_else(PoisonError::into_inner);
        f(&config)
    }

    /// 就地更新（`config` 工具写盘成功后同步内存）。
    pub fn update(&self, f: impl FnOnce(&mut Config)) {
        let mut config = self.0.write().unwrap_or_else(PoisonError::into_inner);
        f(&mut config);
    }
}

impl std::fmt::Debug for SharedConfig {
    /// 不打印整份配置（含密钥），只标出类型。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SharedConfig")
    }
}

impl Config {
    pub fn load_from(path: &Path) -> Result<Option<Config>, ConfigError> {
        match fs::read_to_string(path) {
            Ok(text) => Ok(Some(toml::from_str(&text)?)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save_to(&self, path: &Path) -> Result<(), ConfigError> {
        write_config_text(path, &toml::to_string_pretty(self)?)
    }

    /// 非空的环境变量优先，其次配置文件。
    pub fn resolve_api_key(&self, provider_id: &str, env_value: Option<String>) -> Option<String> {
        env_value.filter(|v| !v.trim().is_empty()).or_else(|| {
            self.providers
                .get(provider_id)
                .and_then(|p| p.api_key.clone())
                .filter(|k| !k.trim().is_empty())
        })
    }

    pub fn set_api_key(&mut self, provider_id: &str, key: String) {
        self.providers
            .entry(provider_id.to_string())
            .or_default()
            .api_key = Some(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            Config::load_from(&dir.path().join("config.toml"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn save_then_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/config.toml");
        let mut cfg = Config::default();
        cfg.set_api_key("deepseek", "sk-abc".into());
        cfg.ui.show_reasoning = true;
        cfg.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path).unwrap(), Some(cfg));
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("[providers.deepseek]"));
        assert!(text.contains("api_key = \"sk-abc\""));
    }

    #[cfg(unix)]
    #[test]
    fn saved_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        Config::default().save_to(&path).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn partial_file_fills_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "model = \"deepseek-v4-pro\"\n").unwrap();
        let cfg = Config::load_from(&path).unwrap().unwrap();
        assert_eq!(cfg.provider, "deepseek");
        assert_eq!(cfg.model, "deepseek-v4-pro");
        assert_eq!(cfg.tools.bash.deny.len(), DEFAULT_DENY.len());
        // 没写到的上限用默认值（老配置文件也能直接读）
        assert_eq!(cfg.agent.max_steps, 50);
        assert_eq!(cfg.tools.bash.default_timeout, 120);
        assert_eq!(cfg.tools.bash.max_timeout, 600);
        assert_eq!(cfg.tools.bash.max_output, 30_000);
        assert_eq!(cfg.tools.read.default_lines, 2000);
        assert_eq!(cfg.tools.search.default_results, 200);
        assert_eq!(cfg.tools.web.max_results, 10);
        assert_eq!(cfg.tools.web.page_chars, 30_000);
    }

    #[test]
    fn user_deny_list_replaces_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "[tools.bash]\ndeny = [\"npm publish\"]\n").unwrap();
        let cfg = Config::load_from(&path).unwrap().unwrap();
        assert_eq!(cfg.tools.bash.deny, vec!["npm publish".to_string()]);
    }

    #[test]
    fn invalid_toml_is_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "model = [").unwrap();
        assert!(matches!(
            Config::load_from(&path),
            Err(ConfigError::Parse(_))
        ));
    }

    #[test]
    fn env_key_takes_precedence() {
        let mut cfg = Config::default();
        assert_eq!(cfg.resolve_api_key("deepseek", None), None);
        cfg.set_api_key("deepseek", "from-file".into());
        assert_eq!(
            cfg.resolve_api_key("deepseek", None).as_deref(),
            Some("from-file")
        );
        assert_eq!(
            cfg.resolve_api_key("deepseek", Some("from-env".into()))
                .as_deref(),
            Some("from-env")
        );
        assert_eq!(
            cfg.resolve_api_key("deepseek", Some("  ".into()))
                .as_deref(),
            Some("from-file")
        );
        assert_eq!(cfg.resolve_api_key("other", None), None);
    }

    #[test]
    fn debug_masks_api_key() {
        let mut cfg = Config::default();
        cfg.set_api_key("deepseek", "sk-1234567890".into());
        let rendered = format!("{cfg:?}");
        assert!(!rendered.contains("sk-1234567890"), "{rendered}");
        assert!(rendered.contains("sk-1…"), "{rendered}");
    }

    #[test]
    fn debug_hides_short_api_key() {
        let mut cfg = Config::default();
        cfg.set_api_key("deepseek", "sk-a".into());
        let rendered = format!("{cfg:?}");
        assert!(!rendered.contains("sk-a"), "{rendered}");
        assert!(rendered.contains("****"), "{rendered}");
    }
    #[test]
    fn web_config_defaults_and_masking() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "[tools.web]\napi_key = \"as_sk_secret123\"\n").unwrap();
        let cfg = Config::load_from(&path).unwrap().unwrap();
        assert_eq!(cfg.tools.web.provider, "anysearch");
        assert_eq!(cfg.tools.web.api_key.as_deref(), Some("as_sk_secret123"));
        let rendered = format!("{cfg:?}");
        assert!(!rendered.contains("as_sk_secret123"), "{rendered}");
        assert!(rendered.contains("as_s…"), "{rendered}");
        assert_eq!(Config::default().tools.web.provider, "anysearch");
    }
}

#[test]
fn shared_config_reads_updates_and_shares() {
    let shared = SharedConfig::new(Config::default());
    assert_eq!(shared.read(|c| c.agent.max_steps), 50);
    shared.update(|c| c.agent.max_steps = 5);
    assert_eq!(shared.read(|c| c.agent.max_steps), 5);
    // 克隆共享同一份内存
    let clone = shared.clone();
    clone.update(|c| c.tools.bash.max_output = 1000);
    assert_eq!(shared.read(|c| c.tools.bash.max_output), 1000);
}

#[test]
fn is_config_file_compares_real_paths() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, "").unwrap();
    assert!(is_config_file(&path, &path));
    assert!(is_config_file(&dir.path().join("./config.toml"), &path));
    #[cfg(unix)]
    {
        let link = dir.path().join("link.toml");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(is_config_file(&link, &path), "符号链接要指向同一文件");
    }
    assert!(!is_config_file(&dir.path().join("other.toml"), &path));
    // 目标还不存在时退回字面比较
    let missing = dir.path().join("nope/config.toml");
    assert!(is_config_file(&missing, &missing));
}

#[test]
fn command_mentions_config_covers_shell_spellings() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".seanbot/config.toml");
    let cwd = tempfile::tempdir().unwrap();
    for command in [
        format!("cat {}", path.display()),
        format!("echo x > {}", path.display()),
        "cat ~/.seanbot/config.toml".into(),
        "cat $HOME/.seanbot/config.toml".into(),
        r"type %USERPROFILE%\.seanbot\config.toml".into(),
        "cat $SEANBOT_HOME/config.toml".into(),
        "cat ${SEANBOT_HOME}/config.toml".into(),
        r"cat $env:SEANBOT_HOME\config.toml".into(),
        "sed -i s/x/y/ /USERS/ME/.SEANBOT/CONFIG.TOML".into(),
    ] {
        assert!(
            command_mentions_config(&command, &path, cwd.path()),
            "应命中：{command}"
        );
    }
    // 工作目录就是数据目录时，相对写法也算
    assert!(command_mentions_config(
        "echo x > config.toml",
        &path,
        path.parent().unwrap()
    ));
    // 普通命令与别处的 config.toml 不受影响
    for command in [
        "cargo test",
        "cat config.toml",
        "ls ~/.seanbot",
        "grep -r key ./src",
    ] {
        assert!(
            !command_mentions_config(command, &path, cwd.path()),
            "不该命中：{command}"
        );
    }
}
