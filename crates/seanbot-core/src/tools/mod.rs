//! 内置工具。

mod bash;
mod edit;
mod read;
mod search;

pub use bash::BashTool;
pub use edit::EditTool;
pub use read::ReadTool;
pub use search::SearchTool;

use std::sync::Arc;

use crate::{registry::ToolRegistry, tool::Tool};

/// 内置工具登记表。内置工具最先注册，名称随之锁定。
pub fn builtin_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::default();
    let tools: [Arc<dyn Tool>; 4] = [
        Arc::new(BashTool),
        Arc::new(EditTool),
        Arc::new(ReadTool),
        Arc::new(SearchTool),
    ];
    for tool in tools {
        registry.register(tool).expect("内置工具名不应冲突");
    }
    registry
}
