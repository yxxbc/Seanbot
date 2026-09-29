//! `config` 工具：查看与修改 `config.toml` 里的上限与默认值。
//!
//! 配置只允许两条修改途径：本工具（改动会经过用户确认）与用户手动编辑文件。
//! 其它内置工具一律拒绝触碰配置文件——`edit`/`read` 在 `ToolContext::is_config_file`
//! 处挡下，`bash` 由 `config::bash_config_guard` 按命令文本挡下，`search` 跳过该文件。

use std::{fs, path::Path};

use async_trait::async_trait;
use seanbot_provider::ToolSpec;
use serde_json::{Value, json};
use toml_edit::{DocumentMut, Item, Table};

use crate::{
    config::{self, Config},
    tool::{Risk, Tool, ToolContext, ToolError, ToolOutput, str_arg},
};

/// 可修改的键（密钥、厂商/模型与 bash 黑名单不在其中：只能由用户手动编辑）。
const EDITABLE: &[(&str, &str)] = &[
    ("agent.max_steps", "1-1000"),
    (
        "tools.bash.default_timeout",
        "1-86400，且不大于 max_timeout",
    ),
    ("tools.bash.max_timeout", "1-86400"),
    ("tools.bash.max_output", "100-1000000 字符"),
    ("tools.read.default_lines", "1-1000000 行"),
    ("tools.kb.max_results", "1-1000 条"),
    ("tools.search.default_results", "1-100000 条"),
    ("tools.web.max_results", "1-50 条"),
    ("tools.web.page_chars", "1000-1000000 字符"),
];

pub struct ConfigTool;

#[async_trait]
impl Tool for ConfigTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "config".into(),
            description: "查看或修改 Seanbot 的配置（`config.toml` 里的工具上限与默认值）。action=list 列出当前值与取值范围；get 读一个键；set 修改一个键（对当前会话立即生效）；unset 删除键、恢复默认值。密钥、厂商/模型与 bash 黑名单不能在这里改，只能由用户手动编辑配置文件。修改需要用户确认。".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["list", "get", "set", "unset"], "description": "list 列出全部可改键；get 读取一个键；set 设置一个键；unset 恢复默认"},
                    "key": {"type": "string", "description": "配置键，如 tools.bash.max_output"},
                    "value": {"type": ["integer", "string", "boolean"], "description": "set 时的值"}
                },
                "required": ["action"]
            }),
        }
    }

    fn risk(&self) -> Risk {
        Risk::Mutating
    }

    /// `list`/`get` 只是读配置，不必让用户确认。
    fn read_only_call(&self, args: &Value) -> bool {
        matches!(
            args.get("action").and_then(Value::as_str),
            Some("list") | Some("get")
        )
    }

    fn title(&self, args: &Value) -> String {
        let action = args.get("action").and_then(Value::as_str).unwrap_or("");
        let key = args.get("key").and_then(Value::as_str);
        match (action, key, args.get("value")) {
            ("set", Some(key), Some(value)) => format!("set {key} = {value}"),
            ("unset", Some(key), _) => format!("unset {key}"),
            ("get", Some(key), _) => format!("get {key}"),
            _ => action.to_string(),
        }
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let action = str_arg(&args, "action")?.trim().to_string();
        match action.as_str() {
            "list" => Ok(output(list(ctx)?, "列出配置")),
            "get" => get(ctx, str_arg(&args, "key")?),
            "set" => {
                let value = args
                    .get("value")
                    .ok_or_else(|| ToolError::InvalidArgs("set 需要 value".into()))?;
                set(ctx, str_arg(&args, "key")?, value)
            }
            "unset" => unset(ctx, str_arg(&args, "key")?),
            other => Err(ToolError::InvalidArgs(format!(
                "未知 action：{other}（可用 list、get、set、unset）"
            ))),
        }
    }
}

fn output(content: String, summary: &str) -> ToolOutput {
    let preview = content.lines().take(3).map(String::from).collect();
    ToolOutput {
        content,
        summary: summary.to_string(),
        preview,
        is_error: false,
    }
}

fn config_file(ctx: &ToolContext) -> Result<std::path::PathBuf, ToolError> {
    ctx.config_path.clone().ok_or_else(|| {
        ToolError::Failed("取不到数据目录（SEANBOT_HOME / 用户主目录），无法读写配置文件".into())
    })
}

/// 当前生效的取值（内存里的那一份；`set` 之后与文件一致）。
fn current(ctx: &ToolContext) -> Config {
    ctx.config.read(|c| c.clone())
}

fn list(ctx: &ToolContext) -> Result<String, ToolError> {
    let path = config_file(ctx)?;
    let cfg = current(ctx);
    let mut lines = vec![
        format!("配置文件：{}", path.display()),
        String::new(),
        "可修改（action=set / unset，改完立即生效）：".into(),
    ];
    for (key, range) in EDITABLE {
        let value = read_key(&cfg, key).unwrap_or(Value::Null);
        lines.push(format!("- {key} = {value}（{range}）"));
    }
    lines.push(String::new());
    lines.push(format!(
        "只读：厂商 {}、模型 {}、联网服务 {}（密钥 {}）、bash 黑名单 {} 条；这些与密钥只能由用户手动编辑配置文件或运行 `sean config`",
        cfg.provider,
        cfg.model,
        cfg.tools.web.provider,
        if cfg.tools.web.api_key.is_some() {
            "已配置"
        } else {
            "未配置"
        },
        cfg.tools.bash.deny.len()
    ));
    Ok(lines.join("\n"))
}

fn get(ctx: &ToolContext, key: &str) -> Result<ToolOutput, ToolError> {
    let (_, range) = spec_of(key).ok_or_else(|| ToolError::InvalidArgs(unknown_key(key)))?;
    let value = read_key(&current(ctx), key).unwrap_or(Value::Null);
    Ok(output(
        format!("{key} = {value}（{range}）\n修改：action=set + value；恢复默认：action=unset"),
        &format!("{key} = {value}"),
    ))
}

fn set(ctx: &ToolContext, key: &str, value: &Value) -> Result<ToolOutput, ToolError> {
    spec_of(key).ok_or_else(|| ToolError::InvalidArgs(unknown_key(key)))?;
    let path = config_file(ctx)?;
    // 先改副本校验，通过后再落盘并同步内存，任何一步失败都不会留下半成品
    let mut cfg = load(&path)?;
    write_key(&mut cfg, key, value).map_err(ToolError::InvalidArgs)?;
    patch_file(&path, key, Some(value))?;
    let now = read_key(&cfg, key).unwrap_or(Value::Null);
    ctx.config.update(|c| *c = cfg);
    Ok(output(
        format!(
            "已更新 {key} = {now}（立即生效）\n配置文件：{}",
            path.display()
        ),
        &format!("set {key} = {now}"),
    ))
}

fn unset(ctx: &ToolContext, key: &str) -> Result<ToolOutput, ToolError> {
    spec_of(key).ok_or_else(|| ToolError::InvalidArgs(unknown_key(key)))?;
    let path = config_file(ctx)?;
    let mut cfg = load(&path)?;
    reset_key(&mut cfg, key);
    patch_file(&path, key, None)?;
    let now = read_key(&cfg, key).unwrap_or(Value::Null);
    ctx.config.update(|c| *c = cfg);
    Ok(output(
        format!(
            "已删除 {key}，恢复默认值 {now}\n配置文件：{}",
            path.display()
        ),
        &format!("unset {key}"),
    ))
}

fn spec_of(key: &str) -> Option<&'static (&'static str, &'static str)> {
    EDITABLE.iter().find(|(k, _)| *k == key)
}

fn unknown_key(key: &str) -> String {
    if key.contains("api_key") {
        return format!(
            "{key} 是密钥，不能通过本工具读写；请手动编辑配置文件，或运行 `sean config`"
        );
    }
    if key == "tools.bash.deny" {
        return "bash 黑名单是安全护栏，不能通过本工具修改；只能由用户手动编辑配置文件".into();
    }
    format!(
        "不支持的配置键：{key}。可改的键：{}",
        EDITABLE
            .iter()
            .map(|(k, _)| *k)
            .collect::<Vec<_>>()
            .join("、")
    )
}

/// 读一个可修改键的当前值。
fn read_key(cfg: &Config, key: &str) -> Option<Value> {
    Some(match key {
        "agent.max_steps" => json!(cfg.agent.max_steps),
        "tools.bash.default_timeout" => json!(cfg.tools.bash.default_timeout),
        "tools.bash.max_timeout" => json!(cfg.tools.bash.max_timeout),
        "tools.bash.max_output" => json!(cfg.tools.bash.max_output),
        "tools.kb.max_results" => json!(cfg.tools.kb.max_results),
        "tools.read.default_lines" => json!(cfg.tools.read.default_lines),
        "tools.search.default_results" => json!(cfg.tools.search.default_results),
        "tools.web.max_results" => json!(cfg.tools.web.max_results),
        "tools.web.page_chars" => json!(cfg.tools.web.page_chars),
        _ => return None,
    })
}

/// 校验并写入一个可修改键。
fn write_key(cfg: &mut Config, key: &str, value: &Value) -> Result<(), String> {
    let int = |low: u64, high: u64| -> Result<u64, String> {
        let n = value
            .as_u64()
            .ok_or_else(|| format!("{key} 需要一个整数，收到 {value}"))?;
        if !(low..=high).contains(&n) {
            return Err(format!("{key} 必须在 {low} 到 {high} 之间，收到 {n}"));
        }
        Ok(n)
    };
    match key {
        "agent.max_steps" => cfg.agent.max_steps = int(1, 1000)? as u32,
        "tools.bash.default_timeout" => cfg.tools.bash.default_timeout = int(1, 86_400)?,
        "tools.bash.max_timeout" => cfg.tools.bash.max_timeout = int(1, 86_400)?,
        "tools.bash.max_output" => cfg.tools.bash.max_output = int(100, 1_000_000)? as usize,
        "tools.kb.max_results" => cfg.tools.kb.max_results = int(1, 1_000)?,
        "tools.read.default_lines" => cfg.tools.read.default_lines = int(1, 1_000_000)?,
        "tools.search.default_results" => cfg.tools.search.default_results = int(1, 100_000)?,
        "tools.web.max_results" => cfg.tools.web.max_results = int(1, 50)?,
        "tools.web.page_chars" => cfg.tools.web.page_chars = int(1_000, 1_000_000)? as usize,
        other => return Err(unknown_key(other)),
    }
    if cfg.tools.bash.default_timeout > cfg.tools.bash.max_timeout {
        return Err(format!(
            "tools.bash.default_timeout（{}）不能大于 tools.bash.max_timeout（{}）",
            cfg.tools.bash.default_timeout, cfg.tools.bash.max_timeout
        ));
    }
    Ok(())
}

/// 恢复默认值（`unset` 用；默认值一定合法，不再校验）。
fn reset_key(cfg: &mut Config, key: &str) {
    let d = Config::default();
    match key {
        "agent.max_steps" => cfg.agent.max_steps = d.agent.max_steps,
        "tools.bash.default_timeout" => {
            cfg.tools.bash.default_timeout = d.tools.bash.default_timeout
        }
        "tools.bash.max_timeout" => cfg.tools.bash.max_timeout = d.tools.bash.max_timeout,
        "tools.bash.max_output" => cfg.tools.bash.max_output = d.tools.bash.max_output,
        "tools.kb.max_results" => cfg.tools.kb.max_results = d.tools.kb.max_results,
        "tools.read.default_lines" => cfg.tools.read.default_lines = d.tools.read.default_lines,
        "tools.search.default_results" => {
            cfg.tools.search.default_results = d.tools.search.default_results
        }
        "tools.web.max_results" => cfg.tools.web.max_results = d.tools.web.max_results,
        "tools.web.page_chars" => cfg.tools.web.page_chars = d.tools.web.page_chars,
        _ => {}
    }
}

/// 从文件读配置（文件不存在或为空则用默认值）。
fn load(path: &Path) -> Result<Config, ToolError> {
    match fs::read_to_string(path) {
        Ok(text) => {
            toml::from_str(&text).map_err(|e| ToolError::Failed(format!("配置文件格式有误：{e}")))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(ToolError::Failed(format!("读取配置文件失败：{e}"))),
    }
}

/// 只改这一项并保留其余内容与原排版（注释不丢）；`None` 表示删掉该键、回到默认值。
fn patch_file(path: &Path, key: &str, value: Option<&Value>) -> Result<(), ToolError> {
    let text = fs::read_to_string(path).unwrap_or_default();
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| ToolError::Failed(format!("配置文件格式有误：{e}")))?;
    let changed = match value {
        Some(value) => set_in_document(&mut doc, key, value),
        None => remove_document_key(&mut doc, key),
    };
    changed.map_err(|e| ToolError::Failed(format!("写入配置失败：{e}")))?;
    config::write_config_text(path, &doc.to_string())
        .map_err(|e| ToolError::Failed(format!("保存配置文件失败：{e}")))
}

/// 在 TOML 文档里就地设置 `key`（中间表不存在时自动创建）。
fn set_in_document(doc: &mut DocumentMut, key: &str, value: &Value) -> Result<(), String> {
    let mut parts = key.split('.').peekable();
    let mut item: &mut Item = doc.as_item_mut();
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            item[part] = toml_item(value)?;
            return Ok(());
        }
        if !item.as_table().is_some_and(|t| t.contains_key(part)) {
            item[part] = Item::Table(Table::new());
        }
        item = &mut item[part];
    }
    Err(format!("配置键不合法：{key}"))
}

/// 删除文档里的 `key`（表不存在时什么也不做）。
fn remove_document_key(doc: &mut DocumentMut, key: &str) -> Result<(), String> {
    let (table_path, leaf) = key
        .rsplit_once('.')
        .ok_or_else(|| format!("配置键不合法：{key}"))?;
    let mut item: &mut Item = doc.as_item_mut();
    for part in table_path.split('.') {
        match item.get_mut(part) {
            Some(next) => item = next,
            None => return Ok(()),
        }
    }
    if let Some(table) = item.as_table_mut() {
        table.remove(leaf);
    }
    Ok(())
}

/// serde_json 值 → toml_edit 项（只支持整数、字符串、布尔）。
fn toml_item(value: &Value) -> Result<Item, String> {
    Ok(match value {
        Value::Number(n) => {
            let n = n.as_i64().ok_or_else(|| format!("只支持整数，收到 {n}"))?;
            toml_edit::value(n)
        }
        Value::String(s) => toml_edit::value(s.as_str()),
        Value::Bool(b) => toml_edit::value(*b),
        other => return Err(format!("不支持的值类型：{other}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SharedConfig;

    /// 建一个指向临时配置文件的上下文（内存里的配置也来自该文件，与生产一致）。
    fn setup(text: &str) -> (tempfile::TempDir, ToolContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, text).unwrap();
        let mut ctx = ToolContext::new(dir.path().to_path_buf());
        ctx.config_path = Some(path.clone());
        ctx.config = SharedConfig::new(load(&path).unwrap());
        (dir, ctx)
    }

    fn file(dir: &tempfile::TempDir) -> String {
        std::fs::read_to_string(dir.path().join("config.toml")).unwrap()
    }

    async fn call(ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        ConfigTool.call(args, ctx).await
    }

    #[tokio::test]
    async fn list_shows_keys_with_ranges() {
        let (_d, ctx) = setup("");
        let out = call(&ctx, json!({"action": "list"})).await.unwrap();
        for key in [
            "agent.max_steps",
            "tools.bash.max_output",
            "tools.web.page_chars",
        ] {
            assert!(out.content.contains(key), "{}", out.content);
        }
        assert!(out.content.contains("30000"), "{}", out.content);
        assert!(out.content.contains("只读"), "{}", out.content);
    }

    #[tokio::test]
    async fn set_updates_file_and_memory() {
        let (dir, ctx) = setup("provider = \"deepseek\"\n");
        let out = call(
            &ctx,
            json!({"action": "set", "key": "tools.bash.max_output", "value": 5000}),
        )
        .await
        .unwrap();
        assert!(out.content.contains("立即生效"), "{}", out.content);
        assert!(file(&dir).contains("max_output = 5000"), "{}", file(&dir));
        assert!(
            file(&dir).contains("provider = \"deepseek\""),
            "其它键要保留：{}",
            file(&dir)
        );
        assert_eq!(ctx.config.read(|c| c.tools.bash.max_output), 5000);
        let out = call(
            &ctx,
            json!({"action": "get", "key": "tools.bash.max_output"}),
        )
        .await
        .unwrap();
        assert!(out.content.contains("5000"), "{}", out.content);
    }

    #[tokio::test]
    async fn set_keeps_comments_and_other_lines() {
        let (dir, ctx) = setup(
            "# 我的配置\nprovider = \"deepseek\"\n\n[tools.bash]\n# 输出别太长\nmax_output = 20000\n",
        );
        call(
            &ctx,
            json!({"action": "set", "key": "tools.read.default_lines", "value": 500}),
        )
        .await
        .unwrap();
        let text = file(&dir);
        assert!(text.contains("# 我的配置"), "{text}");
        assert!(text.contains("# 输出别太长"), "{text}");
        assert!(text.contains("max_output = 20000"), "{text}");
        assert!(text.contains("default_lines = 500"), "{text}");
    }

    #[tokio::test]
    async fn unset_restores_default() {
        let (dir, ctx) = setup("[tools.bash]\nmax_output = 20000\n");
        let out = call(
            &ctx,
            json!({"action": "unset", "key": "tools.bash.max_output"}),
        )
        .await
        .unwrap();
        assert!(out.content.contains("30000"), "{}", out.content);
        assert_eq!(ctx.config.read(|c| c.tools.bash.max_output), 30_000);
        assert!(!file(&dir).contains("max_output"), "{}", file(&dir));
    }

    #[tokio::test]
    async fn rejects_unknown_secret_and_deny_keys() {
        let (_d, ctx) = setup("");
        for (key, needle) in [
            ("providers.deepseek.api_key", "密钥"),
            ("tools.bash.deny", "黑名单"),
            ("nope.nope", "不支持的配置键"),
        ] {
            let err = call(&ctx, json!({"action": "set", "key": key, "value": "x"}))
                .await
                .unwrap_err();
            assert!(err.to_string().contains(needle), "{key}：{err}");
        }
    }

    #[tokio::test]
    async fn rejects_bad_values_and_keeps_file() {
        let (dir, ctx) = setup("");
        for (key, value, needle) in [
            ("tools.bash.max_output", json!(10), "100"),
            ("agent.max_steps", json!(0), "1"),
            ("agent.max_steps", json!("多"), "整数"),
            ("tools.web.max_results", json!(99), "50"),
        ] {
            let err = call(&ctx, json!({"action": "set", "key": key, "value": value}))
                .await
                .unwrap_err();
            assert!(err.to_string().contains(needle), "{key}：{err}");
        }
        assert_eq!(file(&dir), "", "失败的写入不能落盘");
        let err = call(
            &ctx,
            json!({"action": "set", "key": "tools.bash.default_timeout", "value": 700}),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("max_timeout"), "{err}");
    }

    #[tokio::test]
    async fn missing_config_path_reports_error() {
        let (_d, mut ctx) = setup("");
        ctx.config_path = None;
        let err = call(&ctx, json!({"action": "list"})).await.unwrap_err();
        assert!(err.to_string().contains("数据目录"), "{err}");
    }

    #[tokio::test]
    async fn unknown_action_is_rejected() {
        let (_d, ctx) = setup("");
        let err = call(&ctx, json!({"action": "reset"})).await.unwrap_err();
        assert!(err.to_string().contains("未知 action"), "{err}");
        let err = call(&ctx, json!({"action": "set", "key": "agent.max_steps"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("需要 value"), "{err}");
    }

    #[test]
    fn read_only_actions_do_not_need_confirmation() {
        assert!(ConfigTool.read_only_call(&json!({"action": "list"})));
        assert!(ConfigTool.read_only_call(&json!({"action": "get", "key": "agent.max_steps"})));
        assert!(!ConfigTool.read_only_call(&json!({"action": "set"})));
        assert!(!ConfigTool.read_only_call(&json!({"action": "unset"})));
        assert!(!ConfigTool.read_only_call(&json!({})));
    }

    #[test]
    fn title_describes_the_change() {
        assert_eq!(ConfigTool.title(&json!({"action": "list"})), "list");
        assert_eq!(
            ConfigTool.title(&json!({"action": "set", "key": "a.b", "value": 3})),
            "set a.b = 3"
        );
        assert_eq!(
            ConfigTool.title(&json!({"action": "unset", "key": "a.b"})),
            "unset a.b"
        );
    }
}
