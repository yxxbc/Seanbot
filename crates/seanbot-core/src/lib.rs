//! Seanbot 内核：agent 主循环、工具登记、基础工具、黑名单与配置。不接触终端。

pub mod config;
pub mod tools;

mod agent;
mod denylist;
mod event;
mod permission;
mod prompt;
mod registry;
mod runtime;
mod tool;

pub use agent::{Agent, AgentError, DEFAULT_MAX_STEPS};
pub use denylist::Denylist;
pub use event::{AgentEvent, TurnSummary};
pub use permission::{AllowAll, Decision, NonInteractive, PermissionHandler, PermissionRequest};
pub use prompt::system_prompt;
pub use registry::{RegistryError, ToolRegistry};
pub use runtime::{PermissionMode, RuntimeState, SharedRuntime, shared_runtime};
pub use tool::{
    ReadTracker, Risk, Tool, ToolContext, ToolError, ToolOutput, ToolSource, resolve_path,
};
pub use tools::builtin_registry;
