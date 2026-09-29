//! 内置工具。

mod bash;
mod edit;
mod perceive;
mod read;
mod search;
mod web;

pub use bash::BashTool;
pub(crate) use bash::interpreter_label;
pub use edit::EditTool;
pub use perceive::PerceiveTool;
pub use read::ReadTool;
pub use search::SearchTool;
pub use web::{WebFetchTool, WebSearchTool};

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
