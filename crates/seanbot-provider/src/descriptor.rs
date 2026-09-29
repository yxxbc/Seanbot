//! 内置厂商描述。新增 OpenAI 兼容厂商只需加一条描述。

use crate::ModelInfo;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    OpenAiCompat,
}

/// 厂商差异。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quirks {
    /// 流式 delta 与 assistant 消息中推理内容的字段名。
    pub reasoning_field: &'static str,
    /// 后续请求是否回传历史 assistant 消息的推理内容。
    /// DeepSeek：请求携带 tools 时必须回传全部历史推理内容，否则返回 400。
    pub echo_reasoning: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderDescriptor {
    pub id: &'static str,
    pub display_name: &'static str,
    pub kind: ProviderKind,
    pub base_url: &'static str,
    pub api_key_env: &'static str,
    pub default_model: &'static str,
    pub quirks: Quirks,
}

pub const DEEPSEEK: ProviderDescriptor = ProviderDescriptor {
    id: "deepseek",
    display_name: "DeepSeek",
    kind: ProviderKind::OpenAiCompat,
    base_url: "https://api.deepseek.com",
    api_key_env: "DEEPSEEK_API_KEY",
    default_model: "deepseek-flash",
    quirks: Quirks {
        reasoning_field: "reasoning_content",
        echo_reasoning: true,
    },
};

static BUILTIN: [ProviderDescriptor; 1] = [DEEPSEEK];

pub fn builtin_providers() -> &'static [ProviderDescriptor] {
    &BUILTIN
}

pub fn find_provider(id: &str) -> Option<&'static ProviderDescriptor> {
    BUILTIN.iter().find(|d| d.id == id)
}

/// 未知模型的保守默认上下文长度。
pub const DEFAULT_CONTEXT_WINDOW: u64 = 64_000;

/// 已知模型表：(id, 上下文长度, 是否支持工具)
const KNOWN_MODELS: &[(&str, u64, bool)] = &[
    ("deepseek-flash", 1_000_000, true),
    ("deepseek-v4-pro", 1_000_000, true),
    ("deepseek-chat", 128_000, true),
    ("deepseek-reasoner", 128_000, true),
];

/// 组合模型信息：接口返回的上下文长度优先，其次已知模型表，最后保守默认值。
pub fn model_info(id: &str, api_context_window: Option<u64>) -> ModelInfo {
    let known = KNOWN_MODELS.iter().find(|(k, _, _)| *k == id);
    ModelInfo {
        id: id.to_string(),
        context_window: api_context_window
            .filter(|&c| c > 0)
            .or(known.map(|k| k.1))
            .unwrap_or(DEFAULT_CONTEXT_WINDOW),
        supports_tools: known.map(|k| k.2).unwrap_or(true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_deepseek() {
        let d = find_provider("deepseek").unwrap();
        assert_eq!(d.base_url, "https://api.deepseek.com");
        assert_eq!(d.api_key_env, "DEEPSEEK_API_KEY");
        assert!(find_provider("nope").is_none());
        assert_eq!(builtin_providers().len(), 1);
    }

    #[test]
    fn model_info_prefers_api_then_table_then_default() {
        assert_eq!(model_info("deepseek-flash", Some(500)).context_window, 500);
        assert_eq!(model_info("deepseek-flash", None).context_window, 1_000_000);
        assert_eq!(
            model_info("deepseek-flash", Some(0)).context_window,
            1_000_000
        );
        let unknown = model_info("mystery", None);
        assert_eq!(unknown.context_window, DEFAULT_CONTEXT_WINDOW);
        assert!(unknown.supports_tools);
    }
}
use std::sync::Arc;

use crate::{Provider, openai::OpenAiCompat};

/// 按厂商协议类型构造实现。
pub fn create(desc: &ProviderDescriptor, api_key: impl Into<String>) -> Arc<dyn Provider> {
    match desc.kind {
        ProviderKind::OpenAiCompat => Arc::new(OpenAiCompat::new(desc, api_key)),
    }
}
