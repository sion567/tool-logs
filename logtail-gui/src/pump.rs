//! 泵线程：独占一个 tokio runtime，把 logtail 的会话泵（StreamEvent）转发给 GUI 线程。
//!
//! GUI ↔ 泵只经 std mpsc 通信：
//! - GUI → 泵：`PumpCmd`（OpenLocal / ConnectRemote / Stop）
//! - 泵 → GUI：`(tab_id, StreamEvent)` 批次；每个会话保证恰好一个 `Ended` 收尾
//!   （logtail 的本地泵被 Stop 后只断流不发 Ended，这里统一兜底）
//!
//! `ActiveSession::spawn_local_file` 内部 `tokio::spawn`，必须在 runtime 上下文里调用——
//! 本地打开是同步快路径（`rt.enter()` 供上下文，IO 在 runtime 任务里做），注册 stop_tx 无竞态；
//! 远程连接是异步路径（block_on + 20s 超时，期间其它命令排队，转发不受影响——转发跑在 runtime 任务里）。

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;

use logtail_core::config::ServerConfig;
use logtail_core::source::{LogSource, ModeVariant};
use logtail_core::ssh::session::ActiveSession;
use logtail_core::ssh::StreamEvent;

use crate::remote;

/// 远程连接命令载荷（Box 压枚举尺寸，clippy large_variant）
pub struct RemoteConnect {
    pub id: u64,
    pub server: ServerConfig,
    pub source: LogSource,
    pub mode: ModeVariant,
}

impl RemoteConnect {
    /// 排障日志用：源的可读描述
    fn source_name_for_log(&self) -> String {
        match &self.source {
            LogSource::Docker { container, .. } => format!("docker:{container}"),
            LogSource::File { path, .. } => format!("file:{path}"),
            LogSource::Journalctl { service, .. } => format!("journal:{service}"),
            LogSource::Command { cmd } => format!("cmd:{cmd}"),
            LogSource::Loki { url, query, .. } => format!("loki:{url}/{query}"),
            LogSource::LocalFile { path, .. } => format!("local:{path}"),
        }
    }
}

pub enum PumpCmd {
    OpenLocal { id: u64, path: String, tail: usize },
    ConnectRemote(Box<RemoteConnect>),
    Stop { id: u64 },
}

pub struct Pump {
    pub cmd_tx: Sender<PumpCmd>,
    pub ev_rx: Receiver<(u64, StreamEvent)>,
}

pub fn spawn() -> Pump {
    let (cmd_tx, cmd_rx) = channel::<PumpCmd>();
    let (ev_tx, ev_rx) = channel::<(u64, StreamEvent)>();

    // JoinHandle 丢弃即 detach；GUI 退出时 drop cmd_tx → run() 的 recv() 返回 Err → 线程自退
    let _jh = std::thread::Builder::new()
        .name("logtail-pump".into())
        .spawn(move || run(cmd_rx, ev_tx))
        .expect("启动泵线程");

    Pump { cmd_tx, ev_rx }
}

/// 排障日志（~/.config/logtail/logtail.log）；测试运行不写，避免污染真实排障记录
fn diag(msg: &str) {
    if !cfg!(test) {
        logtail_core::config::append_log(msg);
    }
}

fn run(cmd_rx: Receiver<PumpCmd>, ev_tx: Sender<(u64, StreamEvent)>) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let handle = rt.handle().clone();
    let mut stops: HashMap<u64, tokio::sync::oneshot::Sender<()>> = HashMap::new();
    diag(&format!(
        "logtail-gui 泵线程启动 v{}",
        env!("CARGO_PKG_VERSION")
    ));

    while let Ok(cmd) = cmd_rx.recv() {
        // [v0.1.0 教训] 命令处理 panic 曾直接杀死泵线程（远程连接超时包装在运行时上下文外
        // 求值），之后所有 tab 收不到任何数据且无提示——catch_unwind 兜底，线程必须活着。
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            handle_cmd(&rt, &handle, &mut stops, &ev_tx, cmd)
        }));
        if let Err(p) = outcome {
            diag(&format!("泵线程命令处理 panic（已捕获继续服务）: {p:?}"));
        }
    }
    diag("泵线程退出（GUI 已关闭）");
}

/// 单条命令处理；tokio 资源（timer/IO）只允许在 rt 上下文内创建——
/// 异步工作要么包进 `rt.block_on(async {..})`，要么 `handle.spawn`，要么先 `rt.enter()`。
fn handle_cmd(
    rt: &tokio::runtime::Runtime,
    handle: &tokio::runtime::Handle,
    stops: &mut HashMap<u64, tokio::sync::oneshot::Sender<()>>,
    ev_tx: &Sender<(u64, StreamEvent)>,
    cmd: PumpCmd,
) {
    match cmd {
        PumpCmd::OpenLocal { id, path, tail } => {
            let _guard = rt.enter(); // 供 spawn_local_file 内部的 tokio::spawn
            let (sess, _jh) = ActiveSession::spawn_local_file(path, tail);
            let ActiveSession { stop_tx, rx, .. } = sess;
            stops.insert(id, stop_tx);
            let ev = ev_tx.clone();
            handle.spawn(async move { forward(rx, ev, id).await });
            diag(&format!("本地流已打开 id={id}"));
        }
        PumpCmd::ConnectRemote(rc) => {
            let id = rc.id;
            diag(&format!(
                "连接远程 id={id} {}@{}:{} source={}",
                rc.server.user, rc.server.host, rc.server.port, rc.source_name_for_log()
            ));
            // block_on 期间其它命令排队，转发不受影响（跑在 runtime 任务里）；
            // 超时包装在 connect_session_timed 内部（运行时上下文内）求值——
            // 写在这里的参数位置会在无 reactor 的泵线程上创建 timer → panic（v0.1.0 实测）
            match rt.block_on(remote::connect_session_timed(
                &rc.server,
                &rc.source,
                &rc.mode,
                Duration::from_secs(20),
            )) {
                Ok(sess) => {
                    let ActiveSession { stop_tx, rx, .. } = sess;
                    stops.insert(id, stop_tx);
                    let ev = ev_tx.clone();
                    handle.spawn(async move { forward(rx, ev, id).await });
                    diag(&format!("远程会话已建立 id={id}"));
                }
                Err(e) => {
                    diag(&format!("远程连接失败 id={id}: {e:#}"));
                    let _ = ev_tx.send((id, StreamEvent::Lines(vec![format!("[连接失败] {e:#}")])));
                    let _ = ev_tx.send((id, StreamEvent::Ended));
                }
            }
        }
        PumpCmd::Stop { id } => {
            if let Some(st) = stops.remove(&id) {
                let _ = st.send(()); // 泵自退 → rx 关闭 → forward 补发 Ended
            }
            diag(&format!("已请求停止 id={id}"));
        }
    }
}

/// tokio mpsc → GUI 的 std mpsc；无论 Ended、报错还是泵退出，最终恰好补发一个 Ended。
/// （tokio ≥1.54：recv() 返回 Option<T>，None = 发送端全部关闭）
async fn forward(
    mut rx: tokio::sync::mpsc::Receiver<StreamEvent>,
    ev_tx: Sender<(u64, StreamEvent)>,
    id: u64,
) {
    let mut ended = false;
    while !ended {
        match rx.recv().await {
            Some(StreamEvent::Ended) => ended = true,
            Some(StreamEvent::Lines(lines)) => {
                if ev_tx.send((id, StreamEvent::Lines(lines))).is_err() {
                    return; // GUI 已退出
                }
            }
            None => ended = true, // 泵退出（Stop/自退/崩溃）：给 GUI 兜底收尾
        }
    }
    let _ = ev_tx.send((id, StreamEvent::Ended));
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]

    use super::*;

    /// 停止链路：OpenLocal → 收到回放批次 → 追加实时送达 → Stop → 恰好一个 Ended
    #[test]
    fn 本地打开_追加_停止_链路() {
        let dir = std::env::temp_dir().join(format!("logtail-gui-pump-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.log");
        std::fs::write(&path, "l0\nl1\nl2\n").unwrap();

        let pump = spawn();
        pump.cmd_tx
            .send(PumpCmd::OpenLocal { id: 7, path: path.display().to_string(), tail: 10 })
            .unwrap();

        // 回放批次（泵 300ms 轮询一次，给足 3s）
        let got = recv_until(&pump, |ev| matches!(ev, StreamEvent::Lines(_)), "回放批次");
        match got {
            StreamEvent::Lines(lines) => assert!(lines.contains(&"l0".to_string()), "回放含首行: {lines:?}"),
            _ => unreachable!(),
        }

        // 追加一行 → 实时送达
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "l3").unwrap();
        let got = recv_until(&pump, |ev| matches!(ev, StreamEvent::Lines(_)), "追加行");
        match got {
            StreamEvent::Lines(lines) => assert!(lines.contains(&"l3".to_string()), "{lines:?}"),
            _ => unreachable!(),
        }

        // Stop → 恰好一个 Ended
        pump.cmd_tx.send(PumpCmd::Stop { id: 7 }).unwrap();
        let got = recv_until(&pump, |ev| matches!(ev, StreamEvent::Ended), "Stop 后的 Ended");
        assert!(matches!(got, StreamEvent::Ended));
        drop(pump); // cmd_tx drop → 泵线程自退
        std::fs::remove_dir_all(&dir).ok();
    }

    fn recv_until(pump: &Pump, pred: impl Fn(&StreamEvent) -> bool, what: &str) -> StreamEvent {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            if let Ok((_, ev)) = pump.ev_rx.try_recv()
                && pred(&ev)
            {
                return ev;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("3s 内未等到 {what}");
    }

    /// 回归 v0.1.0：远程连接失败不得 panic 杀死泵线程——
    /// （当时的 bug：timeout() 在运行时上下文外求值 → 泵线程死亡 → 之后所有 tab 无数据）
    #[test]
    fn 远程连接失败_报错送达_泵线程存活() {
        // 占用端口后立即关闭 → 127.0.0.1 上连接立即被拒（快速失败路径）
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let pump = spawn();
        pump.cmd_tx
            .send(PumpCmd::ConnectRemote(Box::new(RemoteConnect {
                id: 9,
                server: logtail_core::config::ServerConfig {
                    name: "t".into(),
                    host: "127.0.0.1".into(),
                    port,
                    user: "u".into(),
                    auth: logtail_core::config::AuthCfg::Password { password: "p".into() },
                    sudo: false,
                    sudo_password: None,
                },
                source: LogSource::File { path: "/x.log".into(), tail: 10 },
                mode: ModeVariant::Full,
            })))
            .unwrap();

        // 期待 [连接失败] 行 + Ended
        let got = recv_until(&pump, |ev| matches!(ev, StreamEvent::Lines(_)), "连接失败行");
        match got {
            StreamEvent::Lines(lines) => {
                assert!(lines.iter().any(|l| l.contains("[连接失败]")), "{lines:?}")
            }
            _ => unreachable!(),
        }
        let got = recv_until(&pump, |ev| matches!(ev, StreamEvent::Ended), "失败后的 Ended");
        assert!(matches!(got, StreamEvent::Ended));

        // 泵线程必须仍存活：后续 OpenLocal 仍能正常回放
        let dir = std::env::temp_dir().join(format!("logtail-gui-pump2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t2.log");
        std::fs::write(&path, "after-fail\n").unwrap();
        pump.cmd_tx
            .send(PumpCmd::OpenLocal { id: 10, path: path.display().to_string(), tail: 5 })
            .unwrap();
        let got = recv_until(&pump, |ev| matches!(ev, StreamEvent::Lines(_)), "失败后的本地回放");
        match got {
            StreamEvent::Lines(lines) => assert!(lines.contains(&"after-fail".to_string()), "{lines:?}"),
            _ => unreachable!(),
        }
        drop(pump);
        std::fs::remove_dir_all(&dir).ok();
    }
}
