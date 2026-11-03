//! 入口：默认 TUI（Day3 起）；`--demo` 保留无头打印模式做冒烟。

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--demo") {
        demo().await
    } else {
        logtail::app::run().await
    }
}

/// 无头冒烟通道（Day2 形态）：行 → parse → buffer → 末尾 40 行着色打印
async fn demo() -> Result<()> {
    use std::time::Duration;

    use logtail::config::server::AuthCfg;
    use logtail::log::{colorize, parse_line, LogBuffer};
    use logtail::source::SourceType;
    use logtail::ssh::{connect, LogStream, StreamEvent};

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
        _ => SourceType::Docker,
    };

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
