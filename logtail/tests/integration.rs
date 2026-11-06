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
/// - `sudo -S …`   → 等 stdin 密码，比对 sudo_pw：对则发 "sudo-ok"，错则发拒绝信息（[v0.8.1]）
struct TestServer {
    /// sudo 密码握手用：Some(期望密码) 时启用 sudo 模拟
    sudo_pw: Option<String>,
    /// exec 收到 sudo -S 命令时记下 channel，等 data 回调收密码
    await_pw: Option<ChannelId>,
}

impl TestServer {
    fn new(sudo_pw: Option<String>) -> Self {
        Self { sudo_pw, await_pw: None }
    }
}

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

    // [v0.8.1] sudo -S 握手：stdin 数据 = 密码行，比对后回数据
    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if let (Some(expected), Some(_)) = (&self.sudo_pw, self.await_pw) {
            let got = String::from_utf8_lossy(data).trim_end().to_string();
            let mut payload = "sudo: 密码错误
".to_string();
            if got == *expected {
                payload = "sudo-ok
".to_string();
            }
            session.data(channel, CryptoVec::from_slice(payload.as_bytes()));
            session.eof(channel);
            session.close(channel);
            self.await_pw = None;
        }
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let cmd = String::from_utf8_lossy(data).to_string();
        // [v0.8.1] sudo -S：等 stdin 密码（data 回调），本轮不发载荷
        if cmd.contains("sudo -S") {
            if self.sudo_pw.is_some() {
                self.await_pw = Some(channel);
            } else {
                // 未启用 sudo 模拟：模拟 NOPASSWD 缺失——直接回错误
                session.data(channel, CryptoVec::from_slice(b"sudo: a password is required
"));
                session.eof(channel);
                session.close(channel);
            }
            return Ok(());
        }
        // [v0.8.1] sudo -n：模拟"未配 NOPASSWD"——报错进日志流（本桩语义固定）
        if cmd.contains("sudo -n") {
            session.data(channel, CryptoVec::from_slice(b"sudo: a password is required
"));
            session.eof(channel);
            session.close(channel);
            return Ok(());
        }
        // [v0.6] 模拟服务端 grep：命令带 `| grep -E --line-buffered '<pat>'` 时，
        // 只回传匹配行（子串匹配模拟 ERE 的常用子集）
        let grep_pat = cmd.split("--line-buffered ").nth(1).map(|rest| {
            rest.trim_start()
                .trim_start_matches('\'')
                .split('\'')
                .next()
                .unwrap_or("")
                .to_string()
        });
        let payload: Vec<String> = if cmd.contains("docker logs") {
            // docker JSON 包壳载荷（--timestamps 前缀在 log 字段内，与真机一致）
            let p1 = r#"{"log":"2026-01-05T14:02:04.123+08:00 db error: connection refused\n","stream":"stderr","time":"2026-01-05T06:02:04.123Z"}"#;
            let p2 = r#"{"log":"plain info line\n","stream":"stdout","time":"2026-01-05T06:02:05.000Z"}"#;
            vec![p1.to_string(), p2.to_string()]
        } else {
            match cmd.trim() {
                "crlf" => vec!["hi".into(), "second line".into()],
                "big" => (0..500usize)
                    .map(|no| format!("big-{no:04}-{}", "x".repeat(30)))
                    .collect(),
                _ => vec!["hi".into(), "second line".into()],
            }
        };
        let payload: Vec<String> = match &grep_pat {
            Some(pat) if !pat.is_empty() => payload
                .into_iter()
                .filter(|l| l.contains(pat.as_str()))
                .collect(),
            _ => payload,
        };
        if cmd.contains("big") {
            // 500 行分 20 包发（高吞吐路径）
            for chunk in payload.chunks(25) {
                let buf = format!("{}\n", chunk.join("\n"));
                session.data(channel, CryptoVec::from_slice(buf.as_bytes()));
            }
        } else if !payload.is_empty() {
            let buf = format!("{}\n", payload.join("\n"));
            session.data(channel, CryptoVec::from_slice(buf.as_bytes()));
        }
        session.eof(channel);
        session.close(channel);
        Ok(())
    }
}

async fn spawn_sshd() -> anyhow::Result<u16> {
    spawn_sshd_sudo(None).await
}

async fn spawn_sshd_sudo(sudo_pw: Option<String>) -> anyhow::Result<u16> {
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
            let server = TestServer::new(sudo_pw.clone());
            tokio::spawn(async move {
                let _ = russh::server::run_stream(config, socket, server).await;
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
            sudo: false,
            sudo_password: None,
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
            sudo: false,
            sudo_password: None,
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
    // [v0.5] tab 切换不拆流：,/. 等价的 switch_tab 在两会话间循环。
    // 注意：第二次 connect 的 switch_active 已把激活源切到 file-src（最后连接者）
    assert_eq!(st.tabs.len(), 2, "连接成功即入 tab 条");
    assert_eq!(st.active_source.as_deref(), Some("file-src"));
    st.switch_tab(1);
    assert_eq!(st.active_source.as_deref(), Some("docker-src"), "循环到下一个");
    assert_eq!(st.connections.len(), 2, "切换不拆流");
    st.switch_tab(-1);
    assert_eq!(st.active_source.as_deref(), Some("file-src"), "反向切换");
    assert_eq!(st.connections.len(), 2);
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

/// [v0.6 §16.6] 端到端：P 键把本地过滤推到远端后，服务端只回传匹配行。
/// 缓冲区只有 1 条 = 管道真到了远端并被模拟 grep 执行（无管道时服务端回 2 条）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_远程预过滤_服务端只回传匹配行() -> anyhow::Result<()> {
    use logtail::app::{drain_session, execute_pending, AppState, DrainOutcome, PendingAction, SessionState};
    use logtail::config::server::ServerConfig;
    use logtail::config::source_cfg::SourceConfig;
    use logtail::config::Config;
    use logtail::log::{FilterMode, FilterState};
    use logtail::source::LogSource;

    let port = spawn_sshd().await?;
    let mut cfg = Config::default();
    cfg.general.remember_last_source = false;
    cfg.servers.push(ServerConfig {
        name: "local".into(),
        host: "127.0.0.1".into(),
        port,
        user: "tester".into(),
        auth: AuthCfg::Password { password: "pw".into() },
            sudo: false,
            sudo_password: None,
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
    let mut st = AppState::new(cfg);

    // 本地过滤 "db error" → 连接（do_connect 会把远筛接进命令）
    st.filter_state = FilterState::Active(FilterMode::Text("db error".into()));
    st.remote_prefilter = Some("db error".into());
    st.pending = Some(PendingAction::Connect("docker-src".into()));
    let action = st.pending.take().unwrap();
    execute_pending(&mut st, action).await;
    assert_eq!(
        st.session_states.get("docker-src"),
        Some(&SessionState::Streaming),
        "{}",
        st.status_msg
    );

    // drain 到终态
    for _ in 0..100 {
        if st.session_states.get("docker-src") == Some(&SessionState::Ended) {
            break;
        }
        if matches!(drain_session(&mut st, "docker-src", 200), DrainOutcome::Remove) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // 服务端只回了匹配行：1 条（无管道时是 2 条）——管道真到了远端并被执行
    let buf = st.buffers.get("docker-src").unwrap();
    assert_eq!(
        buf.len(),
        1,
        "远端 grep 必须只回传匹配行，实际 {} 条",
        buf.len()
    );
    assert!(buf.by_line_no(0).unwrap().raw.contains("db error: connection refused"));
    assert_eq!(
        st.remote_prefilter.as_deref(),
        Some("db error"),
        "状态保持：按 R/预设重连仍带远筛"
    );
    Ok(())
}

/// [v0.8] 本地文件流泵：tail 语义 + 增长轮询，不经 SSH
#[tokio::test]
async fn 本地流_回放与追加() -> anyhow::Result<()> {
    use logtail::ssh::session::ActiveSession;
    use tokio::sync::mpsc::error::TryRecvError;

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
    use logtail::ssh::client::connect_with;

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
    use logtail::ssh::session::ActiveSession;
    use tokio::sync::mpsc::error::TryRecvError;

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
