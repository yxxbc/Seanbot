//! 配置向导与 Provider / Agent 的构造。

use std::{
    io::{self, Write},
    path::Path,
    sync::Arc,
};

use anyhow::{Context, anyhow};
use seanbot_core::{
    Agent, AllowAll, builtin_registry,
    config::{Config, config_path},
};
use seanbot_provider::{
    ModelInfo, Provider, ProviderDescriptor, builtin_providers, create, create_traced,
    find_provider,
};

use crate::format;

pub fn load_config() -> anyhow::Result<Option<Config>> {
    let path = config_path()?;
    Config::load_from(&path).with_context(|| format!("读取 {} 失败", path.display()))
}

pub fn descriptor(cfg: &Config) -> anyhow::Result<&'static ProviderDescriptor> {
    find_provider(&cfg.provider)
        .ok_or_else(|| anyhow!("未知厂商：{}（运行 `sean config` 重新选择）", cfg.provider))
}

fn api_key(cfg: &Config, desc: &ProviderDescriptor) -> Option<String> {
    cfg.resolve_api_key(desc.id, std::env::var(desc.api_key_env).ok())
}

/// 读取配置；没有可用的 API key（首次运行）时进入配置向导。
pub async fn ensure_config() -> anyhow::Result<Config> {
    let cfg = load_config()?.unwrap_or_default();
    let desc = descriptor(&cfg)?;
    if api_key(&cfg, desc).is_some() {
        return Ok(cfg);
    }
    println!("尚未配置 API key，先进行配置。");
    wizard(cfg).await
}

pub fn provider_for(cfg: &Config, trace_path: Option<&Path>) -> anyhow::Result<Arc<dyn Provider>> {
    let desc = descriptor(cfg)?;
    let key = api_key(cfg, desc).ok_or_else(|| anyhow!("缺少 API key，请运行 `sean config`"))?;
    match trace_path {
        Some(path) => Ok(create_traced(desc, key, path)?),
        None => Ok(create(desc, key)),
    }
}

pub fn build_agent(
    cfg: &Config,
    model_override: Option<&str>,
    trace_path: Option<&Path>,
) -> anyhow::Result<Agent> {
    let provider = provider_for(cfg, trace_path)?;
    let model = model_override.unwrap_or(&cfg.model).to_string();
    let cwd = std::env::current_dir()?;
    Ok(Agent::new(
        provider,
        model,
        builtin_registry(cfg),
        Arc::new(cfg.clone()),
        Arc::new(AllowAll),
        cwd,
    ))
}

pub async fn wizard(mut cfg: Config) -> anyhow::Result<Config> {
    let providers = builtin_providers();
    println!("选择厂商：");
    for (i, d) in providers.iter().enumerate() {
        println!("  {}. {}", i + 1, d.display_name);
    }
    let idx = if providers.len() == 1 {
        println!("（仅有一个可选，已选择 {}）", providers[0].display_name);
        0
    } else {
        prompt_choice("输入序号：", providers.len(), None)?
    };
    let desc = &providers[idx];
    let existing = cfg.providers.get(desc.id).and_then(|p| p.api_key.clone());

    let models = loop {
        let hint = if existing.is_some() {
            "API key（直接回车保留现有）："
        } else {
            "API key："
        };
        let input = rpassword::prompt_password(hint)?;
        let key = match (input.trim(), &existing) {
            ("", Some(k)) => k.clone(),
            ("", None) => {
                println!("API key 不能为空");
                continue;
            }
            (k, _) => k.to_string(),
        };
        print!("正在验证…");
        io::stdout().flush()?;
        match create(desc, key.clone()).list_models().await {
            Ok(models) if !models.is_empty() => {
                println!(" ✓");
                cfg.set_api_key(desc.id, key);
                break models;
            }
            Ok(_) => println!(" ✗ 厂商未返回任何模型"),
            Err(e) => println!(" ✗ {e}"),
        }
    };

    let default = models
        .iter()
        .position(|m| m.id == cfg.model)
        .or_else(|| models.iter().position(|m| m.id == desc.default_model));
    let chosen = choose_model(&models, default)?;
    cfg.provider = desc.id.to_string();
    cfg.model = models[chosen].id.clone();
    let path = config_path()?;
    cfg.save_to(&path)?;
    println!("已保存到 {}", path.display());
    Ok(cfg)
}

pub fn choose_model(models: &[ModelInfo], default: Option<usize>) -> io::Result<usize> {
    println!("选择模型：");
    for (i, m) in models.iter().enumerate() {
        let mark = if Some(i) == default {
            "（默认）"
        } else {
            ""
        };
        println!(
            "  {}. {}  上下文 {}{mark}",
            i + 1,
            m.id,
            format::tokens(m.context_window)
        );
    }
    let prompt = if default.is_some() {
        "输入序号（回车选默认）："
    } else {
        "输入序号："
    };
    prompt_choice(prompt, models.len(), default)
}

pub async fn print_models(cfg: &Config) -> anyhow::Result<()> {
    let models = provider_for(cfg, None)?.list_models().await?;
    for m in models {
        let mark = if m.id == cfg.model { "*" } else { " " };
        println!(
            "{mark} {}  上下文 {}",
            m.id,
            format::tokens(m.context_window)
        );
    }
    Ok(())
}

pub(crate) fn prompt_choice(prompt: &str, n: usize, default: Option<usize>) -> io::Result<usize> {
    loop {
        print!("{prompt}");
        io::stdout().flush()?;
        let mut line = String::new();
        if io::stdin().read_line(&mut line)? == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "输入已结束"));
        }
        match parse_choice(&line, n, default) {
            Some(i) => return Ok(i),
            None => println!("请输入 1-{n} 之间的序号"),
        }
    }
}

/// 解析 1 起的序号为 0 起的下标；空输入取默认值。
pub fn parse_choice(input: &str, n: usize, default: Option<usize>) -> Option<usize> {
    let t = input.trim();
    if t.is_empty() {
        return default;
    }
    t.parse::<usize>()
        .ok()
        .filter(|&i| (1..=n).contains(&i))
        .map(|i| i - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_choice_rules() {
        assert_eq!(parse_choice("2\n", 3, None), Some(1));
        assert_eq!(parse_choice(" 1 ", 3, None), Some(0));
        assert_eq!(parse_choice("", 3, Some(2)), Some(2));
        assert_eq!(parse_choice("", 3, None), None);
        assert_eq!(parse_choice("0", 3, None), None);
        assert_eq!(parse_choice("4", 3, None), None);
        assert_eq!(parse_choice("abc", 3, None), None);
    }
}
