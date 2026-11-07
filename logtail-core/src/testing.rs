//! 测试设施（§17）：in-process russh sshd 桩，验证"连接→exec→读行→EOF→Ended"全链路，
//! 不需要真实服务器。仅 feature "test-util" 下编译——core 集成测试与前端 e2e 测试共用。
//!
//! echo 桩：按 exec 的命令字决定载荷（模拟不同的远端输出形态）
//! - `echo`        → 两行（`echo hi; exit 0`）
//! - `crlf`        → CRLF 行尾（Windows 风格 / 某些 agent 输出）
//! - `big`         → 500 行、每包 25 行分 20 次发（高吞吐分包重组）
//! - `sudo -S …`   → 等 stdin 密码，比对 sudo_pw：对则发 "sudo-ok"，错则发拒绝信息（[v0.8.1]）
//! - `… \| grep …` → [v0.6] 模拟服务端 grep：只回传匹配行

use std::sync::Arc;
use std::time::Duration;

use russh::keys::key::KeyPair;
use russh::server::{Auth, Handler, Msg, Session};
use russh::{Channel, ChannelId, CryptoVec};

use crate::config::server::AuthCfg;
use crate::ssh::{connect, LogStream, StreamEvent};

pub struct TestServer {
    /// sudo 密码握手用：Some(期望密码) 时启用 sudo 模拟
    sudo_pw: Option<String>,
    /// exec 收到 sudo -S 命令时记下 channel，等 data 回调收密码
    await_pw: Option<ChannelId>,
}

impl TestServer {
    pub fn new(sudo_pw: Option<String>) -> Self {
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

pub async fn spawn_sshd() -> anyhow::Result<u16> {
    spawn_sshd_sudo(None).await
}

pub async fn spawn_sshd_sudo(sudo_pw: Option<String>) -> anyhow::Result<u16> {
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
pub async fn open_stream(port: u16, cmd: &str) -> anyhow::Result<LogStream> {
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
pub async fn collect_until_ended(mut log: LogStream) -> anyhow::Result<(Vec<String>, bool)> {
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
