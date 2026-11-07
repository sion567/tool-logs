//! 集成测试（§17）：in-process russh sshd（桩在 src/testing.rs，feature "test-util"），
//! 验证"连接→exec→读行→EOF→Ended"全链路，不需要真实服务器。
//! 用例覆盖：基本两行 / CRLF 行尾 / 高吞吐 500 行分包（§18 高吞吐不丢行）/ 泵生命周期
//! / 本地流泵 / sudo 密码流 / TOFU 主机密钥。
//! （前端层 e2e：主循环 drain / 多源并发 / 远程预过滤 → logtail-tui/tests/e2e.rs）
//! 真实 docker/file/journal 三源联测仍需真机（§18）。
//!
//! 注意：本 target 声明 required-features = ["test-util"]（core Cargo.toml）——
//! 裸 `cargo test` 自动跳过；跑全量用 `cargo test --all-features`（CI 即如此）。

#![allow(non_snake_case)] // 用例名含中文段落，非 snake_case 是刻意的

use std::time::Duration;

use logtail_core::config::server::AuthCfg;
use logtail_core::ssh::session::ActiveSession;
use logtail_core::ssh::{connect, LogStream, StreamEvent};
use logtail_core::testing::{collect_until_ended, open_stream, spawn_sshd, spawn_sshd_sudo};
use tokio::sync::mpsc::error::TryRecvError;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn eof_terminates_pump_and_no_line_lost() -> anyhow::Result<()> {
    let port = spawn_sshd().await?;
    let (got, ended) = collect_until_ended(open_stream(port, "echo hi").await?).await?;
    assert!(ended, "必须收到 Ended");
    // 一次批读可能拿到 1..2 行（Data 包边界不定），收齐后必须恰好两行、顺序不乱
    assert_eq!(got, vec!["hi", "second line"], "行序与完整性");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crlf_行尾不残留回车() -> anyhow::Result<()> {
    let port = spawn_sshd().await?;
    let (got, ended) = collect_until_ended(open_stream(port, "crlf").await?).await?;
    assert!(ended);
    assert_eq!(got, vec!["hi", "second line"], "\\r 必须剥掉，否则污染级别检测与显示");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn 高吞吐_500行分包_不丢不乱() -> anyhow::Result<()> {
    let port = spawn_sshd().await?;
    let (got, ended) = collect_until_ended(open_stream(port, "big").await?).await?;
    assert!(ended);
    assert_eq!(got.len(), 500, "行数必须完整（分包重组不丢行）");
    for (i, line) in got.iter().enumerate() {
        assert!(line.starts_with(&format!("big-{i:04}-")), "顺序错位: 第{i}行={line}");
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn 泵_整批转发与EOF自退() -> anyhow::Result<()> {
    let port = spawn_sshd().await?;
    let auth = AuthCfg::Password { password: "pw".into() };
    let connect_fut = connect("127.0.0.1", port, "tester", &auth);
    let sess = match tokio::time::timeout(Duration::from_secs(15), connect_fut).await {
        Ok(r) => r?,
        Err(_) => anyhow::bail!("connect 15s 超时"),
    };
    let ch = sess.channel_open_session().await?;
    ch.exec(true, "echo hi".as_bytes()).await?;
    let (mut session, _jh) = ActiveSession::spawn(sess, LogStream::new(ch));

    let mut got = Vec::new();
    let mut ended = false;
    for _ in 0..300 {
        match session.rx.try_recv() {
            Ok(StreamEvent::Lines(lines)) => got.extend(lines),
            Ok(StreamEvent::Ended) => {
                ended = true;
                break;
            }
            Err(TryRecvError::Empty) => tokio::time::sleep(Duration::from_millis(10)).await,
            Err(TryRecvError::Disconnected) => break,
        }
    }
    assert!(ended, "EOF 后泵必须发一次 Ended 并自退");
    assert_eq!(got, vec!["hi", "second line"]);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn 泵_停止路径_收到Ended() -> anyhow::Result<()> {
    let port = spawn_sshd().await?;
    let auth = AuthCfg::Password { password: "pw".into() };
    let connect_fut = connect("127.0.0.1", port, "tester", &auth);
    let sess = match tokio::time::timeout(Duration::from_secs(15), connect_fut).await {
        Ok(r) => r?,
        Err(_) => anyhow::bail!("connect 15s 超时"),
    };
    let ch = sess.channel_open_session().await?;
    ch.exec(true, "echo hi".as_bytes()).await?;
    let (session, _jh) = ActiveSession::spawn(sess, LogStream::new(ch));

    // 解构出 stop_tx/rx 模拟 §13 的 remove 路径：shutdown 信号 → 泵必须发 Ended 后退出
    let ActiveSession { stop_tx, mut rx, .. } = session;
    let _ = stop_tx.send(());

    let mut ended = false;
    for _ in 0..300 {
        match rx.try_recv() {
            Ok(StreamEvent::Ended) => {
                ended = true;
                break;
            }
            Ok(_) => {} // 停止信号竞速中可能已收到部分数据，不算失败
            Err(TryRecvError::Empty) => tokio::time::sleep(Duration::from_millis(10)).await,
            Err(TryRecvError::Disconnected) => break,
        }
    }
    assert!(ended, "stop 信号必须让泵发一次 Ended");
    Ok(())
}

/// [v0.8] 本地文件流泵：tail 语义 + 增长轮询，不经 SSH
#[tokio::test]
async fn 本地流_回放与追加() -> anyhow::Result<()> {
    let dir = std::env::temp_dir().join(format!("logtail-local-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("app.log");
    std::fs::write(&path, "l1
l2
l3
l4
")?;

    let (mut sess, _jh) = ActiveSession::spawn_local_file(path.to_string_lossy().into_owned(), 2);

    // 首批：末尾 2 行
    let mut got = Vec::new();
    for _ in 0..50 {
        match sess.rx.try_recv() {
            Ok(StreamEvent::Lines(ls)) => got.extend(ls),
            Ok(StreamEvent::Ended) => break,
            Err(TryRecvError::Empty) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            Err(TryRecvError::Disconnected) => break,
        }
    }
    assert_eq!(got, vec!["l3", "l4"], "tail 语义：只回放末尾 2 行");

    // 追加 → 300ms 轮询送达
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().append(true).open(&path)?;
    writeln!(f, "l5")?;
    f.flush()?;
    let mut appended = Vec::new();
    for _ in 0..50 {
        match sess.rx.try_recv() {
            Ok(StreamEvent::Lines(ls)) => appended.extend(ls),
            Err(TryRecvError::Empty) => tokio::time::sleep(std::time::Duration::from_millis(30)).await,
            _ => break,
        }
        if !appended.is_empty() {
            break;
        }
    }
    assert_eq!(appended, vec!["l5"], "追加行必须送达");

    // 停止：stop → 泵退出（rx 关闭）
    let ActiveSession { stop_tx, mut rx, .. } = sess;
    let _ = stop_tx.send(());
    let mut closed = false;
    for _ in 0..50 {
        match rx.try_recv() {
            Err(TryRecvError::Disconnected) => {
                closed = true;
                break;
            }
            _ => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
        }
    }
    assert!(closed, "stop 后泵退出");
    std::fs::remove_dir_all(&dir).ok();
    Ok(())
}

/// [v0.8.1] sudo -S 密码流端到端：密码写 stdin → 服务端 data 回调比对 → 回放数据。
/// 验证 do_connect 的 make_writer 写入 + eof 路径与 do_connect 完全一致的手写序列。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sudo密码流_写入stdin后服务端放行() -> anyhow::Result<()> {
    let port = spawn_sshd_sudo(Some("welcome01!".into())).await?;
    let auth = AuthCfg::Password { password: "pw".into() };
    let sess = tokio::time::timeout(
        Duration::from_secs(15),
        connect("127.0.0.1", port, "tester", &auth),
    )
    .await??;
    let ch = sess.channel_open_session().await?;
    ch.exec(true, "sudo -S -p '' docker logs -f x".as_bytes()).await?;
    // 与 do_connect 完全一致的密码写入路径
    {
        use tokio::io::AsyncWriteExt;
        let mut w = ch.make_writer();
        w.write_all(b"welcome01!
").await?;
    }
    ch.eof().await?;
    let mut log = LogStream::new(ch);

    let mut got = Vec::new();
    let mut ended = false;
    for _ in 0..200 {
        match log.try_next().await? {
            Some(StreamEvent::Lines(lines)) => got.extend(lines),
            Some(StreamEvent::Ended) => {
                ended = true;
                break;
            }
            None => tokio::time::sleep(Duration::from_millis(10)).await,
        }
    }
    log.teardown().await;
    assert!(ended);
    assert_eq!(got, vec!["sudo-ok"], "密码正确后服务端放行回数据");
    Ok(())
}

/// [v0.8.1] sudo -n（无密码模式）在未配 NOPASSWD 的机器上：报错进日志流、自然结束不挂起
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sudo_n_无NOPASSWD时报错不挂起() -> anyhow::Result<()> {
    let port = spawn_sshd().await?; // sudo_pw=None：模拟未启用
    let auth = AuthCfg::Password { password: "pw".into() };
    let sess = tokio::time::timeout(
        Duration::from_secs(15),
        connect("127.0.0.1", port, "tester", &auth),
    )
    .await??;
    let ch = sess.channel_open_session().await?;
    ch.exec(true, "sudo -n docker logs -f x".as_bytes()).await?;
    let mut log = LogStream::new(ch);
    let mut got = Vec::new();
    for _ in 0..200 {
        match log.try_next().await? {
            Some(StreamEvent::Lines(lines)) => got.extend(lines),
            Some(StreamEvent::Ended) => break,
            None => tokio::time::sleep(Duration::from_millis(10)).await,
        }
    }
    log.teardown().await;
    assert!(
        got.iter().any(|l| l.contains("password is required")),
        "sudo -n 报错进日志流: {got:?}"
    );
    Ok(())
}

/// [v0.10 §16.5] TOFU：首连记录指纹 → 同 server 重连成功 → known_hosts 篡改后连接被拒
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tofu_首连记录_重连通过_篡改拒绝() -> anyhow::Result<()> {
    use logtail_core::ssh::client::connect_with;

    let port = spawn_sshd().await?;
    let kh = std::env::temp_dir().join(format!("logtail-kh-{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&kh);
    let auth = AuthCfg::Password { password: "pw".into() };

    // 首连（TOFU 记录）
    connect_with("127.0.0.1", port, "tester", &auth, &kh).await?;
    let recorded = std::fs::read_to_string(&kh)?;
    assert!(recorded.contains(&format!("127.0.0.1:{port}")), "首连记录指纹: {recorded}");

    // 同 sshd 重连（指纹一致）通过
    connect_with("127.0.0.1", port, "tester", &auth, &kh).await?;

    // 篡改指纹 → 拒绝
    std::fs::write(&kh, format!("127.0.0.1:{port} FAKE-FINGERPRINT"))?;
    let msg = match connect_with("127.0.0.1", port, "tester", &auth, &kh).await {
        Ok(_) => anyhow::bail!("篡改后必须拒绝"),
        Err(e) => e.to_string(),
    };
    assert!(msg.contains("指纹不匹配"), "{msg}");
    let _ = std::fs::remove_file(&kh);
    Ok(())
}

/// [v0.10] 本地流轮转自愈：truncate 重写后泵送达新内容（不再卡死旧 pos）
#[tokio::test]
async fn 本地流_轮转截断后送达新内容() -> anyhow::Result<()> {
    let dir = std::env::temp_dir().join(format!("logtail-rot-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("app.log");
    std::fs::write(&path, "old-a
old-b
")?;
    let (mut sess, _jh) = ActiveSession::spawn_local_file(path.to_string_lossy().into_owned(), 5);

    // 排空首批（old-a/old-b）
    for _ in 0..50 {
        match sess.rx.try_recv() {
            Ok(StreamEvent::Lines(_)) => {}
            Err(TryRecvError::Empty) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            _ => break,
        }
    }

    // copytruncate：清空重写
    std::fs::write(&path, "new-1
new-2
")?;
    let mut got = Vec::new();
    for _ in 0..100 {
        match sess.rx.try_recv() {
            Ok(StreamEvent::Lines(ls)) => got.extend(ls),
            Err(TryRecvError::Empty) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
            _ => break,
        }
        if got.contains(&"new-1".to_string()) {
            break;
        }
    }
    assert!(
        got.iter().any(|l| l.contains("new-")),
        "轮转后必须送达新内容: {got:?}"
    );
    sess.shutdown();
    std::fs::remove_dir_all(&dir).ok();
    Ok(())
}
