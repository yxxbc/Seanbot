//! Seanbot 厂商中间层：给定对话与工具定义，流式返回模型输出。

mod descriptor;
mod error;
mod openai;
mod provider;
mod types;

pub use descriptor::{
    DEEPSEEK, DEFAULT_CONTEXT_WINDOW, ProviderDescriptor, ProviderKind, Quirks, builtin_providers,
    create, find_provider, model_info,
};
pub use error::ProviderError;
pub use openai::OpenAiCompat;
pub use provider::{ChunkStream, Provider};
pub use types::*;
