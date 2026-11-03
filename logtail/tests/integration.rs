//! 集成测试（§17 末尾"echo hi; exit 0"桩的落地版）：
//! in-process russh sshd，验证"连接→exec→读行→EOF→Ended"全链路，不需要真实服务器。
//! 真实 docker/file/journal 三源联测仍需真机（§18）。

use std::sync::Arc;
use std::time::Duration;

use logtail::config::server::AuthCfg;
use logtail::ssh::{connect, LogStream, StreamEvent};
use russh::keys::key::KeyPair;
use russh::server::{Auth, Handler, Msg, Session};
use russh::{Channel, ChannelId, CryptoVec};

/// echo 桩：exec 任何命令 → 发两行数据 + EOF + Close（模拟 `echo hi; exit 0`）
struct EchoServer;

#[async_trait::async_trait]
impl Handler for EchoServer {
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
        _data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.data(channel, CryptoVec::from_slice(b"hi\nsecond line\n"));
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
                let _ = russh::server::run_stream(config, socket, EchoServer).await;
            });
        }
    });
    Ok(port)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn eof_terminates_pump_and_no_line_lost() -> anyhow::Result<()> {
    let port = spawn_sshd().await?;
    let auth = AuthCfg::Password { password: "pw".into() };
    let connect_fut = connect("127.0.0.1", port, "tester", &auth);
    let sess = match tokio::time::timeout(std::time::Duration::from_secs(15), connect_fut).await {
        Ok(r) => r?,
        Err(_) => anyhow::bail!("connect 15s 超时——握手卡死"),
    };
    let ch = sess.channel_open_session().await?;
    ch.exec(true, "echo hi".as_bytes()).await?;
    let mut log = LogStream::new(ch);

    // 一次批读可能拿到 1..2 行（Data 包边界不定），收齐后必须恰好两行、顺序不乱
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
    assert!(ended, "必须收到 Ended");
    assert_eq!(got, vec!["hi", "second line"], "行序与完整性");
    Ok(())
}
