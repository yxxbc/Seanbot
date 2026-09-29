//! 会话运行时状态：会变化、因此不能放进系统提示词的信息，由 Agent 与 UI 共同维护。

use std::{
    path::PathBuf,
    sync::{Arc, RwLock},
};

use seanbot_provider::Usage;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PermissionMode {
    /// 只读工具直接执行，其余工具需要用户确认
    #[default]
    Confirm,
    /// 全部直接执行（黑名单仍然生效）
    Yolo,
}

impl PermissionMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Confirm => "确认模式",
            Self::Yolo => "YOLO",
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            Self::Confirm => Self::Yolo,
            Self::Yolo => Self::Confirm,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RuntimeState {
    pub provider: String,
    pub model: String,
    pub permission_mode: PermissionMode,
    pub session_id: Option<String>,
    pub session_path: Option<PathBuf>,
    /// 本会话累计用量
    pub usage: Usage,
}

pub type SharedRuntime = Arc<RwLock<RuntimeState>>;

pub fn shared_runtime(state: RuntimeState) -> SharedRuntime {
    Arc::new(RwLock::new(state))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_labels_and_toggle() {
        assert_eq!(PermissionMode::default(), PermissionMode::Confirm);
        assert_eq!(PermissionMode::Confirm.label(), "确认模式");
        assert_eq!(PermissionMode::Yolo.label(), "YOLO");
        assert_eq!(PermissionMode::Confirm.toggled(), PermissionMode::Yolo);
        assert_eq!(PermissionMode::Yolo.toggled(), PermissionMode::Confirm);
    }
}
