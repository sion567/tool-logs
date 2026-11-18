//! 远程源连接：复用 logtail-core 的 ssh::connect / LogSource::to_command / with_sudo。
//!
//! 与 TUI `app.rs::do_connect` 同一条路径（TOFU 主机密钥校验在 ssh::connect 内自动生效）：
//! connect → 拼命令 → sudo 前缀 → 远筛管道 → exec → spawn 泵。

use anyhow::{Result, anyhow};
use std::time::Duration;

use logtail_core::source::{LogSource, ModeVariant};
use logtail_core::ssh::session::ActiveSession;
use logtail_core::ssh::{LogStream, connect};

/// [v0.2.0] 命令拼装（纯函数，单测锁定顺序）：to_command → sudo 前缀 → 远筛管道。
/// 与 TUI do_connect 完全一致——grep 管道必须接在 sudo 包裹后的【最终命令】尾部。
pub fn build_command(
    source: &LogSource,
    mode: &ModeVariant,
    sudo: bool,
    sudo_pw: Option<&str>,
    prefilter: Option<&str>,
) -> Result<String> {
    let mut cmd = source.to_command(mode)?;
    if sudo {
        cmd = logtail_core::source::with_sudo(&cmd, sudo_pw);
    }
    if let Some(p) = prefilter {
        cmd = logtail_core::source::apply_prefilter(&cmd, p)?;
    }
    Ok(cmd)
}

/// connect_session + 超时包装。
///
/// [回归 v0.1.0] `tokio::time::timeout(...)` 必须在本函数体（异步上下文）内求值——
/// timer 资源创建时就要 reactor；此前 pump 线程把它写在 `Handle::block_on` 的参数位置，
/// 参数在进入运行时上下文【之前】求值 → "there is no reactor running" panic，泵线程死亡。
pub async fn connect_session_timed(
    server: &logtail_core::config::ServerConfig,
    source: &LogSource,
    mode: &ModeVariant,
    prefilter: Option<&str>,
    dur: Duration,
) -> Result<ActiveSession> {
    match tokio::time::timeout(dur, connect_session(server, source, mode, prefilter)).await {
        Ok(res) => res,
        Err(_) => Err(anyhow!("连接超时（{dur:?}）")),
    }
}

/// 建立会话并起泵；调用方拿到 ActiveSession 后取走 rx 转发、持有 stop_tx 用于停止
pub async fn connect_session(
    server: &logtail_core::config::ServerConfig,
    source: &LogSource,
    mode: &ModeVariant,
    prefilter: Option<&str>,
) -> Result<ActiveSession> {
    // [v0.10.9] Keyring 认证在此解析（pump 线程）：GUI 前端内存只见 Keyring 引用，
    // 密码经 SSH 只进 russh/命令；缺条目等错误走既有 [连接失败] 注入路径
    let auth = server.resolved_auth()?;
    let handle = connect(&server.host, server.port, &server.user, &auth).await?;
    // sudo 密码解析收口到 core（显式 sudo_password 优先，否则复用登录密码）
    let sudo_pw = server.resolved_sudo_password()?;
    let cmd = build_command(source, mode, server.sudo, sudo_pw.as_deref(), prefilter)?;

    let ch = handle.channel_open_session().await?;
    ch.exec(true, cmd).await?;
    // sudo -S 密码模式：exec 后立即写密码行并 EOF（防密码错时 sudo 挂起等 stdin）
    if let Some(pw) = &sudo_pw {
        use tokio::io::AsyncWriteExt;
        let mut w = ch.make_writer();
        w.write_all(format!("{pw}\n").as_bytes()).await?;
        let _ = ch.eof().await;
    }

    let (sess, _jh) = ActiveSession::spawn(handle, LogStream::new(ch));
    Ok(sess)
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]

    use super::*;
    use logtail_core::config::{AuthCfg, ServerConfig};

    fn test_server(host: &str, port: u16) -> ServerConfig {
        ServerConfig {
            name: "t".into(),
            host: host.into(),
            port,
            user: "u".into(),
            auth: AuthCfg::Password {
                password: "p".into(),
            },
            sudo: false,
            sudo_password: None,
        }
    }

    /// 回归 v0.1.0：超时包装必须能在「无 tokio 上下文的外部线程 + rt.block_on」里工作，
    /// 不得 panic（"there is no reactor running"）。对不回包地址走稳定超时路径。
    #[test]
    fn 连接超时_外部线程_block_on_不panic() {
        // 监听但不 accept：TCP 握手由内核完成，SSH banner 永不到达 → 稳定走超时
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let source = LogSource::File {
            path: "/tmp/x.log".into(),
            tail: 10,
        };
        let t0 = std::time::Instant::now();
        let out = rt.block_on(connect_session_timed(
            &test_server(&addr.ip().to_string(), addr.port()),
            &source,
            &ModeVariant::Full,
            None,
            Duration::from_millis(300),
        ));
        assert!(t0.elapsed() < Duration::from_secs(5), "应按时超时而非挂死");
        let err = out.expect_err("应超时失败");
        assert!(format!("{err:#}").contains("连接超时"), "{err:#}");
        drop(listener);
    }

    #[test]
    fn 命令拼装_sudo包裹后接远筛管道() {
        let src = LogSource::Docker {
            container: "my-app".into(),
            tail: 100,
            follow: true,
            since: None,
            until: None,
        };
        // 无 sudo 无远筛：原样
        let cmd = build_command(&src, &ModeVariant::Full, false, None, None).unwrap();
        assert!(cmd.contains("docker logs") && cmd.contains("my-app"), "{cmd}");
        // sudo 包裹 + 远筛：grep 接在最终命令尾部（先包 sudo 再接管道）；
        // "db error" 含空格非 safe_ident → shell_quote 加单引号
        let cmd = build_command(
            &src,
            &ModeVariant::Full,
            true,
            Some("pw"),
            Some("db error"),
        )
        .unwrap();
        assert!(
            cmd.ends_with("| grep -E --line-buffered 'db error'"),
            "远筛必须在命令尾: {cmd}"
        );
        assert!(cmd.starts_with("sudo -S -p ''"), "sudo 包裹在最外: {cmd}");
        // Range 模式不带 -f
        let cmd = build_command(
            &src,
            &ModeVariant::Range("2026-01-05 10:00:00".into(), "2026-01-05 11:00:00".into()),
            false,
            None,
            None,
        )
        .unwrap();
        assert!(!cmd.contains(" -f"), "精准窗口禁止 -f: {cmd}");
        // 坏 pattern 被拒，绝不拼接进命令
        let err = build_command(&src, &ModeVariant::Full, false, None, Some("a;b")).unwrap_err();
        assert!(format!("{err:#}").contains("非法"), "{err:#}");
    }

    // [v0.10.9] sudo 密码回退语义（显式优先/空串=未设/复用登录密码/非 sudo 恒无）
    // 随实现收口迁至 core：config::server::tests::resolved_sudo_password_四条回退路径
}
