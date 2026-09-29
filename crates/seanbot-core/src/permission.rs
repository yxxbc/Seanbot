use async_trait::async_trait;
use serde_json::Value;

use crate::{
    runtime::{PermissionMode, SharedRuntime},
    tool::Risk,
};

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

/// `-p` 等非交互场景：只读工具放行；需要确认的工具在 YOLO 下放行，否则拒绝并说明原因。
pub struct NonInteractive {
    runtime: SharedRuntime,
}

impl NonInteractive {
    pub fn new(runtime: SharedRuntime) -> Self {
        Self { runtime }
    }
}

#[async_trait]
impl PermissionHandler for NonInteractive {
    async fn ask(&self, req: PermissionRequest) -> Decision {
        let yolo = self.runtime.read().unwrap().permission_mode == PermissionMode::Yolo;
        if yolo || req.risk == Risk::ReadOnly {
            Decision::AllowOnce
        } else {
            Decision::Deny {
                reason: Some("非交互模式下未授权，可加 --yolo".into()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{PermissionMode, RuntimeState, shared_runtime};

    fn request(risk: Risk) -> PermissionRequest {
        PermissionRequest {
            tool: "t".into(),
            title: String::new(),
            risk,
            args: Value::Null,
        }
    }

    #[tokio::test]
    async fn non_interactive_policy() {
        let rt = shared_runtime(RuntimeState::default());
        let h = NonInteractive::new(rt.clone());
        assert_eq!(h.ask(request(Risk::ReadOnly)).await, Decision::AllowOnce);
        assert_eq!(
            h.ask(request(Risk::Mutating)).await,
            Decision::Deny {
                reason: Some("非交互模式下未授权，可加 --yolo".into())
            }
        );
        rt.write().unwrap().permission_mode = PermissionMode::Yolo;
        assert_eq!(h.ask(request(Risk::Mutating)).await, Decision::AllowOnce);
    }
}
