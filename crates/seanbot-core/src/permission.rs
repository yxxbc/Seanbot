use async_trait::async_trait;
use serde_json::Value;

use crate::tool::Risk;

#[derive(Debug, Clone, PartialEq)]
pub struct PermissionRequest {
    pub tool: String,
    pub title: String,
    pub risk: Risk,
    pub args: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    AllowOnce,
    AllowSession,
    Deny { reason: Option<String> },
}

#[async_trait]
pub trait PermissionHandler: Send + Sync {
    async fn ask(&self, req: PermissionRequest) -> Decision;
}

/// 本版 CLI 使用：全部放行。黑名单在此之前检查，仍然生效。
pub struct AllowAll;

#[async_trait]
impl PermissionHandler for AllowAll {
    async fn ask(&self, _req: PermissionRequest) -> Decision {
        Decision::AllowOnce
    }
}
