//! 集成测试（§17 末尾"echo hi; exit 0"桩的落地版）：
//! in-process russh sshd，验证"连接→exec→读行→EOF→Ended"全链路，不需要真实服务器。
//! 用例覆盖：基本两行 / CRLF 行尾 / 高吞吐 500 行分包（§18 高吞吐不丢行）/ 泵生命周期。
//! 真实 docker/file/journal 三源联测仍需真机（§18）。

#![allow(non_snake_case)] // 用例名含中文段落，非 snake_case 是刻意的

use std::sync::Arc;
use std::time::Duration;

use logtail::config::server::AuthCfg;
use logtail::ssh::session::ActiveSession;
use logtail::ssh::{connect, LogStream, StreamEvent};
use russh::keys::key::KeyPair;
use russh::server::{Auth, Handler, Msg, Session};
use russh::{Channel, ChannelId, CryptoVec};
use tokio::sync::mpsc::error::TryRecvError;

/// echo 桩：按 exec 的命令字决定载荷（模拟不同的远端输出形态）
/// - `echo`        → 两行（`echo hi; exit 0`）
/// - `crlf`        → CRLF 行尾（Windows 风格 / 某些 agent 输出）
/// - `big`         → 500 行、每包 25 行分 20 次发（高吞吐分包重组）
struct TestServer;

#[async_trait::async_trait]
impl Handler for TestServer {
    type Error = russh::Error;

    async fn auth_password(&mut self, _user: &str, _password: &str) -> Result<Auth, Self::Error> {
        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let cmd = String::from_utf8_lossy(data).to_string();
        if cmd.contains("docker logs") {
            // docker JSON 包壳载荷（--timestamps 前缀在 log 字段内，与真机一致）
            let p1 = r#"{"log":"2026-01-05T14:02:04.123+08:00 db error: connection refused\n","stream":"stderr","time":"2026-01-05T06:02:04.123Z"}"#;
            let p2 = r#"{"log":"plain info line\n","stream":"stdout","time":"2026-01-05T06:02:05.000Z"}"#;
            session
                .data(channel, CryptoVec::from_slice(format!("{p1}\n{p2}\n").as_bytes()));
        } else {
            match cmd.trim() {
                "crlf" => {
                    session
                        .data(channel, CryptoVec::from_slice(b"hi\r\nsecond line\r\n"));
                }
                "big" => {
                    // 500 行 × "big-<no>-pad…"（~40 字节），分 20 包发
                    for chunk in 0..20usize {
                        let mut buf = String::new();
                        for i in 0..25usize {
                            let no = chunk * 25 + i;
                            buf.push_str(&format!("big-{no:04}-{}", "x".repeat(30)));
                            buf.push('\n');
                        }
                        session.data(channel, CryptoVec::from_slice(buf.as_bytes()));
                    }
                }
                _ => {
                    session.data(channel, CryptoVec::from_slice(b"hi\nsecond line\n"));
                }
            }
        }
        session.eof(channel);
        session.close(channel);
        Ok(())
    }
}

async fn spawn_sshd() -> anyhow::Result<u16> {
    let host_key = KeyPair::generate_ed25519().expect("生成 host key");
    let config = Arc::new(russh::server::Config {
        keys: vec![host_key],
        ..Default::default()
    });

    // 必须用 tokio listener：std 的阻塞 accept 会堵死 runtime worker，
    // run_stream 任务被饿死 → 客户端永远等不到 server SSH ID
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();

    tokio::spawn(async move {
        loop {
            let (socket, _) = match listener.accept().await {
                Ok(x) => x,
                Err(_) => break,
            };
            let config = config.clone();
            tokio::spawn(async move {
                let _ = russh::server::run_stream(config, socket, TestServer).await;
            });
        }
    });
    Ok(port)
}

/// 连上并 exec 给定命令，返回 LogStream（带 15s 握手超时兜底）
async fn open_stream(port: u16, cmd: &str) -> anyhow::Result<LogStream> {
    let auth = AuthCfg::Password { password: "pw".into() };
    let connect_fut = connect("127.0.0.1", port, "tester", &auth);
    let sess = match tokio::time::timeout(Duration::from_secs(15), connect_fut).await {
        Ok(r) => r?,
        Err(_) => anyhow::bail!("connect 15s 超时——握手卡死"),
    };
    let ch = sess.channel_open_session().await?;
    ch.exec(true, cmd.as_bytes()).await?;
    Ok(LogStream::new(ch))
}

/// 读到 Ended 为止收集全部行（上限轮次防死循环）
async fn collect_until_ended(mut log: LogStream) -> anyhow::Result<(Vec<String>, bool)> {
    let mut got = Vec::new();
    let mut ended = false;
    for _ in 0..2000 {
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
    Ok((got, ended))
}

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

/// 端到端（§3.2 数据流全链路）：config → to_command → SSH exec → 流 → parse → buffer。
/// 验证 docker JSON 包壳在真实 channel 上被正确拆开、时间戳取包壳 time、级别/源/line_no 正确。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_配置到缓冲区_docker_json链路() -> anyhow::Result<()> {
    use logtail::app::{drain_session, execute_pending, AppState, DrainOutcome, PendingAction, SessionState};
    use logtail::config::server::ServerConfig;
    use logtail::config::source_cfg::SourceConfig;
    use logtail::config::{Config, PresetCfg};
    use logtail::log::LogLevel;
    use logtail::source::LogSource;
    use std::collections::BTreeMap;

    let port = spawn_sshd().await?;

    let mut cfg = Config::default();
    cfg.general.remember_last_source = false; // 测试确定性：不读真实 state.toml
    cfg.servers.push(ServerConfig {
        name: "local".into(),
        host: "127.0.0.1".into(),
        port,
        user: "tester".into(),
        auth: AuthCfg::Password { password: "pw".into() },
    });
    cfg.sources.push(SourceConfig {
        name: "docker-src".into(),
        server: "local".into(),
        source: LogSource::Docker {
            container: "my-app".into(),
            tail: 100,
            follow: true,
            since: None,
            until: None,
        },
    });
    let mut presets = BTreeMap::new();
    presets.insert(
        "quick_error".to_string(),
        PresetCfg {
            name: Some("仅ERROR(1h)".into()),
            level: Some("error".into()),
            since: Some("1h".into()),
        },
    );
    cfg.presets = presets;

    let mut st = AppState::new(cfg);

    // 连接（走 pending → execute_pending，与主循环同路径）
    st.pending = Some(PendingAction::Connect("docker-src".into()));
    let action = st.pending.take().unwrap();
    execute_pending(&mut st, action).await;
    assert_eq!(
        st.session_states.get("docker-src"),
        Some(&SessionState::Streaming),
        "连接应成功: {}",
        st.status_msg
    );

    // 消化到终态（drain_session 与主循环同路径）
    for _ in 0..100 {
        match drain_session(&mut st, "docker-src", 200) {
            DrainOutcome::Remove => break,
            _ => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
    assert_eq!(
        st.session_states.get("docker-src"),
        Some(&SessionState::Ended),
        "服务端 EOF 后应到终态"
    );

    // buffer 内容校验
    let buf = st.buffers.get("docker-src").expect("buffer 应随连接创建");
    assert_eq!(buf.len(), 2, "两行 JSON 包壳应拆成两条 LogLine");
    let l0 = buf.by_line_no(0).unwrap();
    assert!(l0.raw.contains("db error: connection refused"), "raw={}", l0.raw);
    assert_eq!(l0.level, LogLevel::Error);
    assert!(l0.timestamp.is_some(), "包壳 time 必须被提取");
    assert_eq!(l0.source, "docker-src", "逻辑源名随入栈写入");
    let l1 = buf.by_line_no(1).unwrap();
    assert!(l1.raw.contains("plain info line"), "raw={}", l1.raw);
    assert_eq!(l1.level, LogLevel::Info);
    Ok(())
}

/// 多源并发（§13 遍历所有 connections / §9 多会话并存）：
/// 两个源同时连接（docker JSON + 普通 tail），各自 drain 到终态，buffer 互不串。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn 多源并发_两会话互不串() -> anyhow::Result<()> {
    use logtail::app::{drain_session, execute_pending, AppState, DrainOutcome, PendingAction, SessionState};
    use logtail::config::server::ServerConfig;
    use logtail::config::source_cfg::SourceConfig;
    use logtail::config::Config;
    use logtail::log::LogLevel;
    use logtail::source::{LogSource, SourceType};

    let port = spawn_sshd().await?;

    let mut cfg = Config::default();
    cfg.general.remember_last_source = false;
    cfg.servers.push(ServerConfig {
        name: "local".into(),
        host: "127.0.0.1".into(),
        port,
        user: "tester".into(),
        auth: AuthCfg::Password { password: "pw".into() },
    });
    cfg.sources.push(SourceConfig {
        name: "docker-src".into(),
        server: "local".into(),
        source: LogSource::Docker {
            container: "my-app".into(),
            tail: 100,
            follow: true,
            since: None,
            until: None,
        },
    });
    cfg.sources.push(SourceConfig {
        name: "file-src".into(),
        server: "local".into(),
        source: LogSource::File { path: "/var/log/x.log".into(), tail: 50 },
    });

    let mut st = AppState::new(cfg);
    for sid in ["docker-src", "file-src"] {
        st.pending = Some(PendingAction::Connect(sid.into()));
        let action = st.pending.take().unwrap();
        execute_pending(&mut st, action).await;
    }
    assert_eq!(st.connections.len(), 2, "两会话并存");
    assert_eq!(st.source_type_of(&"docker-src".to_string()), SourceType::Docker);
    assert_eq!(st.source_type_of(&"file-src".to_string()), SourceType::File);

    // 两源各自 drain 到终态（对齐主循环语义：终态会话跳过，不重复 drain——
    // 否则空队列 + 泵已退 = Disconnected，会把 Ended 翻成 Failed）
    for _ in 0..100 {
        let mut done = 0;
        for sid in ["docker-src", "file-src"] {
            if st.session_states.get(sid) == Some(&SessionState::Ended) {
                done += 1;
                continue;
            }
            if matches!(drain_session(&mut st, sid, 200), DrainOutcome::Remove) {
                done += 1;
            }
        }
        if done == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(st.session_states.get("docker-src"), Some(&SessionState::Ended));
    assert_eq!(st.session_states.get("file-src"), Some(&SessionState::Ended));

    // buffer 内容互不串
    let d = st.buffers.get("docker-src").unwrap();
    assert_eq!(d.len(), 2);
    assert_eq!(d.by_line_no(0).unwrap().level, LogLevel::Error, "docker 源走了 JSON 拆壳");
    let f = st.buffers.get("file-src").unwrap();
    assert_eq!(f.len(), 2);
    assert_eq!(f.by_line_no(0).unwrap().raw, "hi", "file 源原文保留");
    Ok(())
}
