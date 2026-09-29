//! 工具登记表。BTreeMap 保证工具定义顺序固定（前缀缓存依赖于此）。

use std::{collections::BTreeMap, sync::Arc};

use seanbot_provider::ToolSpec;

use crate::tool::{Tool, ToolSource};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RegistryError {
    #[error("工具名 {0} 与内置工具冲突")]
    BuiltinConflict(String),
    #[error("工具名 {0} 已注册")]
    Duplicate(String),
}

#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// 内置工具先注册即锁定名称；之后任何同名注册都会失败。
    pub fn register(&mut self, tool: Arc<dyn Tool>) -> Result<(), RegistryError> {
        let name = tool.spec().name;
        if let Some(existing) = self.tools.get(&name) {
            return Err(if existing.source() == ToolSource::Builtin {
                RegistryError::BuiltinConflict(name)
            } else {
                RegistryError::Duplicate(name)
            });
        }
        self.tools.insert(name, tool);
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|t| t.spec()).collect()
    }

    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::{Risk, ToolContext, ToolError, ToolOutput};
    use async_trait::async_trait;
    use serde_json::{Value, json};

    struct Dummy {
        name: &'static str,
        source: ToolSource,
    }

    #[async_trait]
    impl Tool for Dummy {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.name.into(),
                description: String::new(),
                parameters: json!({}),
            }
        }
        fn source(&self) -> ToolSource {
            self.source.clone()
        }
        fn risk(&self) -> Risk {
            Risk::ReadOnly
        }
        fn title(&self, _: &Value) -> String {
            String::new()
        }
        async fn call(&self, _: Value, _: &ToolContext) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::new("", ""))
        }
    }

    fn dummy(name: &'static str, source: ToolSource) -> Arc<dyn Tool> {
        Arc::new(Dummy { name, source })
    }

    #[test]
    fn specs_sorted_by_name() {
        let mut r = ToolRegistry::default();
        r.register(dummy("search", ToolSource::Builtin)).unwrap();
        r.register(dummy("bash", ToolSource::Builtin)).unwrap();
        r.register(dummy("read", ToolSource::Builtin)).unwrap();
        let names: Vec<_> = r.specs().into_iter().map(|s| s.name).collect();
        assert_eq!(names, ["bash", "read", "search"]);
        assert_eq!(r.names(), ["bash", "read", "search"]);
        assert!(r.get("bash").is_some());
        assert!(r.get("nope").is_none());
    }

    #[test]
    fn builtin_names_are_locked() {
        let mut r = ToolRegistry::default();
        r.register(dummy("bash", ToolSource::Builtin)).unwrap();
        assert_eq!(
            r.register(dummy("bash", ToolSource::Plugin { id: "x".into() })),
            Err(RegistryError::BuiltinConflict("bash".into()))
        );
    }

    #[test]
    fn duplicate_non_builtin_rejected() {
        let mut r = ToolRegistry::default();
        r.register(dummy("mine", ToolSource::AgentCreated)).unwrap();
        assert_eq!(
            r.register(dummy("mine", ToolSource::AgentCreated)),
            Err(RegistryError::Duplicate("mine".into()))
        );
    }
}
