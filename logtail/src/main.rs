//! 入口（§四 Day1 骨架）：连上服务器，把日志流打到本地终端。
//!
//! Day3 起替换为 TUI；本无头模式保留为 `--demo` 冒烟通道。
//!
//! ```bash
//! LOGTAIL_HOST=1.2.3.4 LOGTAIL_USER=root LOGTAIL_PASS=xxx \
//! LOGTAIL_CMD="docker logs -f --tail 100 --timestamps my-app" cargo run -- --demo
//! ```

use std::time::Duration;

use anyhow::Result;
use logtail::config::server::AuthCfg;
use logtail::ssh::{connect, LogStream, StreamEvent};

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if !args.iter().any(|a| a == "--demo") {
        eprintln!("logtail v0.4.0：目前只有 --demo 无头模式（Day3 起 TUI）");
        eprintln!("  LOGTAIL_HOST/LOGTAIL_PORT/LOGTAIL_USER/LOGTAIL_PASS/LOGTAIL_CMD 环境变量配置目标");
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

    let sess = connect(&host, port, &user, &AuthCfg::Password { password: pass }).await?;
    let ch = sess.channel_open_session().await?;
    ch.exec(true, cmd).await?;

    let mut log = LogStream::new(ch);
    loop {
        match log.try_next().await? {
            // [v0.4 P0#2] 整批打印，不丢行
            Some(StreamEvent::Lines(lines)) => {
                for l in lines {
                    println!("{l}");
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
