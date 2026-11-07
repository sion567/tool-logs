//! `--demo` 无头模式（Day2 形态）：连上服务器把着色日志流打印到终端，
//! 不进 TUI——管道/脚本里好用，也是最小化验证 SSH 通道的工具。

use std::time::Duration;

use anyhow::Result;
use logtail_core::config::server::AuthCfg;
use logtail_core::log::{colorize, parse_line, LogBuffer};
use logtail_core::source::SourceType;
use logtail_core::ssh::{connect, LogStream, StreamEvent};

pub async fn run() -> Result<()> {
    fn env_or(k: &str, d: &str) -> String {
        std::env::var(k).unwrap_or_else(|_| d.to_string())
    }

    let host = env_or("LOGTAIL_HOST", "192.168.1.100");
    let port: u16 = std::env::var("LOGTAIL_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(22);
    let user = env_or("LOGTAIL_USER", "root");
    let pass = env_or("LOGTAIL_PASS", "");
    let cmd = env_or(
        "LOGTAIL_CMD",
        "docker logs -f --tail 100 --timestamps my-container",
    );
    let src_type = match env_or("LOGTAIL_SRC", "docker").as_str() {
        "file" => SourceType::File,
        "journal" => SourceType::Journalctl,
        "local" => SourceType::LocalFile,
        _ => SourceType::Docker,
    };

    // 本地文件：不走 SSH，直接本地泵
    if let SourceType::LocalFile = src_type {
        return local_tail(&cmd, 500).await;
    }

    let sess = connect(&host, port, &user, &AuthCfg::Password { password: pass }).await?;
    let ch = sess.channel_open_session().await?;
    ch.exec(true, cmd).await?;

    let mut log = LogStream::new(ch);
    let mut buffer = LogBuffer::new(10_000);
    loop {
        match log.try_next().await? {
            // [v0.4 P0#2] 整批 parse+push，不丢行
            Some(StreamEvent::Lines(lines)) => {
                for raw in lines {
                    buffer.push(parse_line(raw, src_type));
                }
                println!("[2J[H");
                for line in buffer.tail(40) {
                    println!("{}", colorize(line));
                }
            }
            Some(StreamEvent::Ended) => {
                println!("--- ended ---");
                break;
            }
            None => tokio::time::sleep(Duration::from_millis(30)).await,
        }
    }
    log.teardown().await;
    Ok(())
}

/// 本地文件冒烟：tail 语义打印
async fn local_tail(path: &str, tail: usize) -> Result<()> {
    let (mut sess, _jh) = logtail_core::ssh::session::ActiveSession::spawn_local_file(path.into(), tail);
    loop {
        match sess.rx.try_recv() {
            Ok(StreamEvent::Lines(lines)) => {
                for l in lines {
                    println!("{}", colorize(&logtail_core::log::LogLine {
                        raw: l,
                        ..Default::default()
                    }));
                }
            }
            Ok(StreamEvent::Ended) => {
                println!("--- ended ---");
                break;
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                tokio::time::sleep(Duration::from_millis(200)).await
            }
            Err(_) => break,
        }
    }
    Ok(())
}
