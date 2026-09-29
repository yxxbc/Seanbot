//! Seanbot 内核：agent 主循环、工具登记、基础工具、黑名单与配置。不接触终端。

pub mod bash_session;
pub mod config;
pub mod instruction;
pub mod kb;
pub mod session;
pub mod skill;
pub mod tools;
pub mod web;

mod agent;
mod denylist;
mod embedded;
mod event;
mod permission;
mod prompt;
mod registry;
mod runtime;
mod tool;

pub use agent::{Agent, AgentError, DEFAULT_MAX_STEPS};
pub use denylist::Denylist;
pub use event::{AgentEvent, TurnSummary, UserEvent};
pub use permission::{AllowAll, Decision, NonInteractive, PermissionHandler, PermissionRequest};
pub use prompt::{PromptEnv, system_prompt};
pub use registry::{RegistryError, ToolRegistry};
pub use runtime::{PermissionMode, RuntimeState, SharedRuntime, shared_runtime};
pub use tool::{
    FileSnapshot, ReadTracker, Risk, Tool, ToolContext, ToolError, ToolOutput, ToolSource,
    is_within, resolve_path,
};
pub use tools::builtin_registry;
