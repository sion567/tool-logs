//! 入口：`--demo` 无头模式（Day2 形态：行 → parse → buffer → 末尾 40 行着色打印）。
//! Day3 起默认进 TUI。

use std::time::Duration;

use anyhow::Result;
use logtail::config::server::AuthCfg;
use logtail::log::{colorize, parse_line, LogBuffer};
use logtail::source::SourceType;
use logtail::ssh::{connect, LogStream, StreamEvent};

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if !args.iter().any(|a| a == "--demo") {
        eprintln!("logtail v0.4.0：目前只有 --demo 无头模式（Day3 起 TUI）");
        eprintln!(
            "  LOGTAIL_HOST/LOGTAIL_PORT/LOGTAIL_USER/LOGTAIL_PASS/LOGTAIL_CMD/LOGTAIL_SRC 环境变量配置目标"
        );
        return Ok(());
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
        _ => SourceType::Docker,
    };

    let sess = connect(&host, port, &user, &AuthCfg::Password { password: pass }).await?;
    let ch = sess.channel_open_session().await?;
    ch.exec(true, cmd).await?;

    // [Day2] 行 → parse → buffer.push → 末尾 40 行着色打印
    let mut log = LogStream::new(ch);
    let mut buffer = LogBuffer::new(10_000);
    loop {
        match log.try_next().await? {
            // [v0.4 P0#2] 整批 parse+push，不丢行
            Some(StreamEvent::Lines(lines)) => {
                for raw in lines {
                    buffer.push(parse_line(raw, src_type));
                }
                println!("\x1b[2J\x1b[H");
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

fn env_or(k: &str, d: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| d.to_string())
}
