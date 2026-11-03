//! 会话泵（§7.4）：LogStream → mpsc，EOF/停止 自退 + teardown

use std::fmt;
use std::time::Duration;

use crate::ssh::client::ClientHandler;
use crate::ssh::stream::{LogStream, StreamEvent};

pub struct ActiveSession {
    pub handle: russh::client::Handle<ClientHandler>,
    pub stop_tx: tokio::sync::oneshot::Sender<()>,
    /// [§13] 泵发事件、主循环 try_recv 消费
    pub rx: tokio::sync::mpsc::Receiver<StreamEvent>,
}

// russh Handle 未实现 Debug；手动给个骨架即可（会话内部状态不进日志）
impl fmt::Debug for ActiveSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ActiveSession").finish_non_exhaustive()
    }
}

impl ActiveSession {
    /// 把一个已 exec 的 channel 变成"后台泵"：读流 → 发 rx，EOF 也发一次 Ended 后自退
    pub fn spawn(
        session: russh::client::Handle<ClientHandler>,
        mut log: LogStream,
    ) -> (Self, tokio::task::JoinHandle<()>) {
        let (tx, rx) = tokio::sync::mpsc::channel::<StreamEvent>(1024);
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();

        let handle = session;
        let jh = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut stop_rx => { let _ = tx.send(StreamEvent::Ended).await; break; }
                    res = log.try_next() => match res {
                        Ok(Some(ev)) => {
                            let is_end = matches!(ev, StreamEvent::Ended);
                            let _ = tx.send(ev).await;
                            if is_end { break; }
                        }
                        // 无数据（超时/未成行）：让出，继续轮询
                        Ok(None) => tokio::time::sleep(Duration::from_millis(10)).await,
                        Err(_) => { let _ = tx.send(StreamEvent::Ended).await; break; }
                    }
                }
            }
            // [v0.3 P2#10] EOF/报错/停止都走 teardown，不留孤儿进程；
            // 连接本体由 Handle 的 Drop 兜底关闭
            log.teardown().await;
        });

        (Self { handle, stop_tx, rx }, jh)
    }

    /// 外部请求断开：通知泵退出，泵内 teardown 关 channel
    pub fn shutdown(self) {
        let _ = self.stop_tx.send(());
    }
}
