//! `~/.seanbot/config.toml` 的读写。

use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
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
    pub tools: ToolsConfig,
    pub ui: UiConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            provider: "deepseek".into(),
            model: "deepseek-flash".into(),
            providers: BTreeMap::new(),
            tools: ToolsConfig::default(),
            ui: UiConfig::default(),
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BashConfig {
    pub deny: Vec<String>,
}

impl Default for BashConfig {
    fn default() -> Self {
        Self {
            deny: DEFAULT_DENY.iter().map(|s| s.to_string()).collect(),
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

impl Config {
    pub fn load_from(path: &Path) -> Result<Option<Config>, ConfigError> {
        match fs::read_to_string(path) {
            Ok(text) => Ok(Some(toml::from_str(&text)?)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save_to(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(self)?;
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
}
