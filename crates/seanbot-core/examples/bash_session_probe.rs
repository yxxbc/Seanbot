//! 常驻 bash 会话的清理探针：给 scripts/tests/bash_session_test.sh 用。
//!
//! 三种退出方式，覆盖清理的四层保证：
//! - `drop`      ：正常返回 → BashSessions::drop 里杀进程组
//! - `exit`      ：`std::process::exit` 跳过 Drop → 靠 stdin 管道 EOF 让 shell 自杀
//! - `closeall`  ：显式 close_all
//!
//! 用法：`cargo run -q -p seanbot-core --example bash_session_probe -- <drop|exit|closeall> <会话数>`
//! 输出：每个会话一行 `PROBE pid=<pid> name=<名字>`，供脚本核对进程是否真的消失。

use std::{process::ExitCode, time::Duration};

use seanbot_core::bash_session::BashSessions;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "drop".into());
    let count: usize = args
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(2);

    let sessions = BashSessions::new(8);
    let cancel = CancellationToken::new();

    for index in 0..count {
        let name = format!("probe{index}");
        // 会话里留一个长期后台进程：清理时必须连它一起收掉
        let output = sessions
            .run(
                &name,
                "cd /tmp && export PROBE_VAR=kept && sleep 300 & echo $!",
                Duration::from_secs(10),
                4096,
                &cancel,
            )
            .await
            .expect("会话里执行命令失败");
        let background = output
            .output
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_string();
        let pid = sessions
            .list()
            .into_iter()
            .find(|item| item.name == name)
            .and_then(|item| item.pid);
        println!(
            "PROBE name={name} pid={} background={background} cwd={}",
            pid.map(|value| value.to_string())
                .unwrap_or_else(|| "?".into()),
            output.cwd
        );
    }

    match mode.as_str() {
        "exit" => {
            // 跳过所有析构：父进程直接消失。子 shell 的 stdin 管道会关闭，
            // shell 读到 EOF 自行退出（第三层兜底）。
            std::process::exit(0);
        }
        "closeall" => {
            let closed = sessions.close_all();
            println!("PROBE closed={}", closed.join(","));
        }
        _ => println!("PROBE dropping"),
    }

    drop(sessions);
    ExitCode::SUCCESS
}
