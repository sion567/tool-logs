//! TUI 层 e2e 集成测试（§17）：主循环路径（execute_pending / drain_session）在真实
//! in-process sshd（桩来自 logtail-core 的 testing 模块）上的端到端验证。
//! 纯 core 层（连接/泵/解析/TOFU/sudo）用例见 logtail-core/tests/integration.rs。
//!
//! 用例覆盖：docker JSON 拆壳全链路 / 多源并发互不串 / 远程预过滤（服务端只回传匹配行）。

#![allow(non_snake_case)] // 用例名含中文段落，非 snake_case 是刻意的

use std::time::Duration;

use logtail_core::config::server::AuthCfg;
use logtail_core::testing::spawn_sshd;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_配置到缓冲区_docker_json链路() -> anyhow::Result<()> {
    use logtail_core::config::server::ServerConfig;
    use logtail_core::config::source_cfg::SourceConfig;
    use logtail_core::config::{Config, PresetCfg};
    use logtail_core::log::LogLevel;
    use logtail_core::source::LogSource;
    use logtail_tui::app::{drain_session, execute_pending, AppState, DrainOutcome, PendingAction, SessionState};
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
    use logtail_core::config::server::ServerConfig;
    use logtail_core::config::source_cfg::SourceConfig;
    use logtail_core::config::Config;
    use logtail_core::log::LogLevel;
    use logtail_core::source::{LogSource, SourceType};
    use logtail_tui::app::{drain_session, execute_pending, AppState, DrainOutcome, PendingAction, SessionState};

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
    use logtail_core::config::server::ServerConfig;
    use logtail_core::config::source_cfg::SourceConfig;
    use logtail_core::config::Config;
    use logtail_core::log::{FilterMode, FilterState};
    use logtail_core::source::LogSource;
    use logtail_tui::app::{drain_session, execute_pending, AppState, DrainOutcome, PendingAction, SessionState};

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
