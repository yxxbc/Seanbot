mod format;
mod render;
mod repl;
mod setup;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "sean", version, about = "Seanbot —— 终端里的 AI 助手")]
struct Cli {
    /// 单次提问：打印回答后退出
    #[arg(short = 'p', long = "print", value_name = "问题")]
    prompt: Option<String>,

    /// 临时覆盖本次使用的模型
    #[arg(long, global = true, value_name = "模型")]
    model: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// 配置厂商、API key 与模型
    Config,
    /// 列出当前厂商的模型
    Models,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("错误：{e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    match cli.command {
        Some(Command::Config) => {
            setup::wizard(setup::load_config()?.unwrap_or_default()).await?;
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Models) => {
            let cfg = setup::ensure_config().await?;
            setup::print_models(&cfg).await?;
            Ok(ExitCode::SUCCESS)
        }
        None => {
            let cfg = setup::ensure_config().await?;
            let mut agent = setup::build_agent(&cfg, cli.model.as_deref())?;
            match cli.prompt {
                Some(prompt) => Ok(repl::run_once(&mut agent, &cfg, prompt).await),
                None => {
                    repl::run(&mut agent, &cfg).await?;
                    Ok(ExitCode::SUCCESS)
                }
            }
        }
    }
}
