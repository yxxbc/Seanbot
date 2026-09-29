//! Seanbot 内核：agent 主循环、工具登记、基础工具、黑名单与配置。不接触终端。

pub mod config;
mod registry;
mod tool;

pub use registry::{RegistryError, ToolRegistry};
pub use tool::{
    ReadTracker, Risk, Tool, ToolContext, ToolError, ToolOutput, ToolSource, resolve_path,
};
