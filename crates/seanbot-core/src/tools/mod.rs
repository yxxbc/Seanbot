//! 内置工具。

mod bash;
mod config;
mod edit;
mod kb;
mod perceive;
mod read;
mod search;
mod web;

pub use bash::BashTool;
pub(crate) use bash::interpreter_label;
pub use config::ConfigTool;
pub use edit::EditTool;
pub use kb::{KbAddTool, KbEditTool, KbListTool, KbSearchTool, KbUpdateTool};
pub use perceive::PerceiveTool;
pub use read::ReadTool;
pub use search::SearchTool;
pub use web::{WebFetchTool, WebSearchTool};

use std::sync::Arc;

use crate::{config::Config, registry::ToolRegistry, tool::Tool};

/// 内置工具登记表。内置工具最先注册，名称随之锁定。
pub fn builtin_registry(config: &Config) -> ToolRegistry {
    let web = crate::web::backend_from_config(config);
    let mut registry = ToolRegistry::default();
    let tools: [Arc<dyn Tool>; 13] = [
        Arc::new(BashTool),
        Arc::new(ConfigTool),
        Arc::new(EditTool),
        Arc::new(KbAddTool),
        Arc::new(KbEditTool),
        Arc::new(KbListTool),
        Arc::new(KbSearchTool),
        Arc::new(KbUpdateTool),
        Arc::new(PerceiveTool),
        Arc::new(ReadTool),
        Arc::new(SearchTool),
        Arc::new(WebFetchTool::new(web.clone())),
        Arc::new(WebSearchTool::new(web)),
    ];
    for tool in tools {
        registry.register(tool).expect("内置工具名不应冲突");
    }
    registry
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn registry_contains_all_builtin_tools_in_order() {
        let names = builtin_registry(&Config::default()).names();
        assert_eq!(
            names,
            [
                "bash",
                "config",
                "edit",
                "kb_add",
                "kb_edit",
                "kb_list",
                "kb_search",
                "kb_update",
                "perceive",
                "read",
                "search",
                "web_fetch",
                "web_search"
            ]
        );
    }
}
