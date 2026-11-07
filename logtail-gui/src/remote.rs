//! 远程源连接：复用 logtail 的 ssh::connect / LogSource::to_command / with_sudo。
//!
//! 与 TUI `app.rs::do_connect` 同一条路径（TOFU 主机密钥校验在 ssh::connect 内自动生效）：
//! connect → 拼命令 → sudo 前缀 → exec → spawn 泵。区别只在不含 remote_prefilter（GUI v1 不做远筛）。

use anyhow::{anyhow, Result};
use std::time::Duration;

use logtail_core::config::AuthCfg;
use logtail_core::source::{LogSource, ModeVariant};
use logtail_core::ssh::session::ActiveSession;
use logtail_core::ssh::{connect, LogStream};

/// connect_session + 超时包装。
///
/// [回归 v0.1.0] `tokio::time::timeout(...)` 必须在本函数体（异步上下文）内求值——
/// timer 资源创建时就要 reactor；此前 pump 线程把它写在 `Handle::block_on` 的参数位置，
/// 参数在进入运行时上下文【之前】求值 → "there is no reactor running" panic，泵线程死亡。
pub async fn connect_session_timed(
    server: &logtail_core::config::ServerConfig,
    source: &LogSource,
    mode: &ModeVariant,
    dur: Duration,
) -> Result<ActiveSession> {
    match tokio::time::timeout(dur, connect_session(server, source, mode)).await {
        Ok(res) => res,
        Err(_) => Err(anyhow!("连接超时（{dur:?}）")),
    }
}

/// 建立会话并起泵；调用方拿到 ActiveSession 后取走 rx 转发、持有 stop_tx 用于停止
pub async fn connect_session(
    server: &logtail_core::config::ServerConfig,
    source: &LogSource,
    mode: &ModeVariant,
) -> Result<ActiveSession> {
    let handle = connect(&server.host, server.port, &server.user, &server.auth).await?;
    let mut cmd = source.to_command(mode)?;

    // sudo 前缀（与 app.rs::do_connect 一致）：密码模式 sudo_password 未填时复用登录密码
    let sudo_pw: Option<String> = if server.sudo {
        Some(
            server
                .sudo_password
                .clone()
                .or_else(|| match &server.auth {
                    AuthCfg::Password { password } => Some(password.clone()),
                    _ => None,
                })
                .unwrap_or_default(),
        )
        .filter(|s| !s.is_empty())
    } else {
        None
    };
    if server.sudo {
        cmd = logtail_core::source::with_sudo(&cmd, sudo_pw.as_deref());
    }

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
    use logtail_core::config::ServerConfig;

    fn test_server(host: &str, port: u16) -> ServerConfig {
        ServerConfig {
            name: "t".into(),
            host: host.into(),
            port,
            user: "u".into(),
            auth: AuthCfg::Password { password: "p".into() },
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
        let source = LogSource::File { path: "/tmp/x.log".into(), tail: 10 };
        let t0 = std::time::Instant::now();
        let out = rt.block_on(connect_session_timed(
            &test_server(&addr.ip().to_string(), addr.port()),
            &source,
            &ModeVariant::Full,
            Duration::from_millis(300),
        ));
        assert!(t0.elapsed() < Duration::from_secs(5), "应按时超时而非挂死");
        let err = out.expect_err("应超时失败");
        assert!(format!("{err:#}").contains("连接超时"), "{err:#}");
        drop(listener);
    }
}
