//! 常驻 bash 会话的行为与清理测试（Unix）。
//!
//! 这里覆盖行为；"绝不留孤儿进程"的系统级验证在 scripts/tests/bash_session_test.sh。

#![cfg(unix)]

use std::time::Duration;

use seanbot_core::bash_session::{BashSessions, SessionError};
use tokio_util::sync::CancellationToken;

fn cancel() -> CancellationToken {
    CancellationToken::new()
}

async fn run(sessions: &BashSessions, name: &str, command: &str) -> String {
    sessions
        .run(name, command, Duration::from_secs(10), 8192, &cancel())
        .await
        .expect("命令应当执行成功")
        .output
}

/// 进程是否还活着。
fn alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

async fn wait_gone(pid: u32) -> bool {
    for _ in 0..50 {
        if !alive(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    !alive(pid)
}

#[tokio::test]
async fn state_persists_inside_a_session_and_sessions_are_isolated() {
    let sessions = BashSessions::new(4);
    let out = run(&sessions, "one", "cd /tmp && export KEEP=yes").await;
    assert!(out.trim().is_empty(), "{out}");
    let out = run(&sessions, "one", "pwd && echo $KEEP").await;
    assert!(out.contains("/tmp"), "cd 没保留：{out}");
    assert!(out.contains("yes"), "环境变量没保留：{out}");

    let out = run(&sessions, "two", "pwd && echo [$KEEP] && echo done").await;
    assert!(out.contains("done"), "{out}");
    assert!(!out.contains("yes"), "会话之间不该共享状态：{out}");

    sessions.close_all();
}

#[tokio::test]
async fn exit_code_and_cwd_come_back() {
    let sessions = BashSessions::new(2);
    let output = sessions
        .run(
            "codes",
            // 用子 shell 退出：在常驻会话里直接 exit 会把会话本身关掉（那是另一种情况）
            "cd /tmp && (exit 7)",
            Duration::from_secs(5),
            4096,
            &cancel(),
        )
        .await
        .unwrap();
    assert_eq!(output.exit_code, Some(7), "{output:?}");
    assert!(output.cwd.ends_with("/tmp"), "{output:?}");
    sessions.close_all();
}

#[tokio::test]
async fn close_kills_the_session() {
    let sessions = BashSessions::new(4);
    run(&sessions, "work", "sleep 300 & echo $!").await;
    let session_pid = sessions.list()[0].pid.expect("会话应有 pid");

    assert!(sessions.close("work").unwrap());
    assert!(!sessions.close("work").unwrap(), "重复关闭应返回 false");
    assert!(
        wait_gone(session_pid).await,
        "会话进程没被收掉：{session_pid}"
    );
    assert!(sessions.list().is_empty());
}

#[tokio::test]
async fn close_all_clears_everything() {
    let sessions = BashSessions::new(4);
    run(&sessions, "a", "sleep 300 & echo $!").await;
    run(&sessions, "b", "sleep 300 & echo $!").await;
    let pids: Vec<u32> = sessions
        .list()
        .into_iter()
        .filter_map(|item| item.pid)
        .collect();
    assert_eq!(pids.len(), 2);

    let closed = sessions.close_all();
    assert_eq!(closed, vec!["a".to_string(), "b".to_string()]);
    for pid in pids {
        assert!(wait_gone(pid).await, "会话进程没被收掉：{pid}");
    }
    assert!(sessions.list().is_empty());
}

#[tokio::test]
async fn dropping_the_registry_cleans_up() {
    let pid = {
        let sessions = BashSessions::new(2);
        run(&sessions, "temp", "sleep 300 & echo $!").await;
        let pid = sessions.list()[0].pid.expect("会话应有 pid");
        assert!(alive(pid));
        pid
    };
    assert!(wait_gone(pid).await, "Drop 没清理会话：{pid}");
}

#[tokio::test]
async fn timeout_closes_the_session_instead_of_leaking_it() {
    let sessions = BashSessions::new(2);
    run(&sessions, "slow", "sleep 300 & echo $!").await;
    let pid = sessions.list()[0].pid.unwrap();

    let output = sessions
        .run(
            "slow",
            "sleep 30",
            Duration::from_millis(300),
            4096,
            &cancel(),
        )
        .await
        .unwrap();
    assert!(output.timed_out, "{output:?}");
    assert!(sessions.list().is_empty(), "超时后会话应当被关闭");
    assert!(wait_gone(pid).await, "超时后会话进程应被收掉：{pid}");
}

#[tokio::test]
async fn timeout_in_a_fresh_session_does_not_leave_a_process() {
    let sessions = BashSessions::new(2);
    // 会话不存在时超时：创建出来的会话也必须被收掉
    let output = sessions
        .run(
            "fresh",
            "sleep 30",
            Duration::from_millis(300),
            4096,
            &cancel(),
        )
        .await
        .unwrap();
    assert!(output.timed_out, "{output:?}");
    assert!(sessions.list().is_empty());
}

#[tokio::test]
async fn limits_and_names_are_enforced() {
    let sessions = BashSessions::new(1);
    run(&sessions, "first", "true").await;
    let err = sessions
        .run("second", "true", Duration::from_secs(5), 4096, &cancel())
        .await
        .unwrap_err();
    assert!(matches!(err, SessionError::TooMany(1)), "{err}");
    sessions.close_all();

    for bad in ["", "A", "-x", "有中文", "a b"] {
        let err = sessions
            .run(bad, "true", Duration::from_secs(5), 4096, &cancel())
            .await
            .unwrap_err();
        assert!(matches!(err, SessionError::BadName(_)), "{bad}: {err}");
    }
}
