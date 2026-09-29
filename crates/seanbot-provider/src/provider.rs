use async_trait::async_trait;
use futures::stream::BoxStream;

use crate::{ChatRequest, ModelInfo, ProviderError, ProviderInfo, StreamChunk};

pub type ChunkStream = BoxStream<'static, Result<StreamChunk, ProviderError>>;

#[async_trait]
pub trait Provider: Send + Sync {
    fn info(&self) -> &ProviderInfo;
    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError>;
    /// 发起一次流式对话。工具调用分片由实现内部拼装，只输出完整的 `ToolCall`。
    async fn stream(&self, req: ChatRequest) -> Result<ChunkStream, ProviderError>;
}
