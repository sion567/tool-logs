//! 会话泵（§7.4）：LogStream → mpsc，EOF/停止 自退 + teardown

use std::fmt;
use std::time::Duration;

use crate::ssh::client::ClientHandler;
use crate::ssh::stream::{LogStream, StreamEvent};

/// 会话生命周期状态（§9，[v0.3 P2#10]）。
/// [core 化] 原定义在 TUI app.rs；TUI 与 GUI 都要区分这四态，故下沉核心。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SessionState {
    Connecting,
    Streaming,
    Ended,
    Failed,
}

impl SessionState {
    /// [v0.8.4→v0.8.7] tab 条状态标注：
    /// - 流中：`*` 前缀（最常见，保持简洁）
    /// - 非流中：中文状态词（用户反馈单字符 x/c 看不懂）
    ///
    /// 全中文/ASCII 混排无 ambiguous 宽度符号，不引入 v0.8.2 的错位问题
    pub fn tab_label(self) -> &'static str {
        match self {
            SessionState::Connecting => "·连接中",
            SessionState::Streaming => "",
            SessionState::Ended => "·已结束",
            SessionState::Failed => "·失败",
        }
    }

    pub fn row_label(self) -> &'static str {
        match self {
            SessionState::Connecting => "连接中",
            SessionState::Streaming => "*",
            SessionState::Ended => "已结束",
            SessionState::Failed => "失败",
        }
    }
}

pub struct ActiveSession {
    /// [v0.8] SSH 会话句柄；本地流源（LocalFile）为 None（断开时无需 disconnect）
    pub handle: Option<russh::client::Handle<ClientHandler>>,
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

        let handle = Some(session);
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

    /// [v0.8 §20] 本地文件流泵：tail 语义（先回放末尾 N 行，之后 300ms 轮询增长）。
    /// 不经 SSH——`local` 源的本地流双模（过滤/详情/导出与远端源完全一致）。
    /// 打开失败不 panic：错误行 + Ended 发给主循环，状态栏可见。
    pub fn spawn_local_file(
        path: String,
        tail: usize,
    ) -> (Self, tokio::task::JoinHandle<()>) {
        use crate::ssh::stream::LineBuffer;
        let (tx, rx) = tokio::sync::mpsc::channel::<StreamEvent>(1024);
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();

        let jh = tokio::spawn(async move {
            let mut lb = LineBuffer::default();
            let res = std::fs::File::open(&path);
            if let Err(e) = res {
                let _ = tx
                    .send(StreamEvent::Lines(vec![format!("[本地文件打开失败] {e}")]))
                    .await;
                let _ = tx.send(StreamEvent::Ended).await;
                return;
            }
            let mut file = res.unwrap();
            use std::io::{Read, Seek, SeekFrom};
            // 回放末尾 64KB（tail 行数近似；不足处按整行计）
            let len = file.metadata().map(|m| m.len()).unwrap_or(0);
            let start_pos = len.saturating_sub(64 * 1024);
            let mut head = String::new();
            if start_pos > 0 {
                // 跳过被截断的半行
                let _ = file.seek(SeekFrom::Start(start_pos));
                let mut probe = vec![0u8; 512];
                const NL_BYTE: u8 = 10;
                let n = file.read(&mut probe).unwrap_or(0);
                if let Some(pos) = probe[..n].iter().position(|&b| b == NL_BYTE) {
                    let _ = file.seek(SeekFrom::Start(start_pos + pos as u64 + 1));
                    head.push_str(&String::from_utf8_lossy(&probe[pos + 1..n]));
                }
            }
            let mut rest = Vec::new();
            let _ = file.read_to_end(&mut rest);
            head.push_str(&String::from_utf8_lossy(&rest));
            let mut lines = lb.push(head.as_bytes());
            let skip = lines.len().saturating_sub(tail);
            let batch: Vec<String> = lines.drain(..).skip(skip).collect();
            if !batch.is_empty() {
                let _ = tx.send(StreamEvent::Lines(batch)).await;
            }
            let mut pos = file.stream_position().unwrap_or(len);
            // [v0.10] 轮转指纹：文件开头 64 字节。copytruncate 后 len 可能恰好 == pos
            //（len<pos 检测漏掉），开头指纹变化才是可靠信号
            let _ = file.seek(SeekFrom::Start(0));
            let mut start_fp = vec![0u8; 64];
            let n = file.read(&mut start_fp).unwrap_or(0);
            start_fp.truncate(n);
            let _ = file.seek(SeekFrom::Start(pos));
            loop {
                tokio::select! {
                    _ = &mut stop_rx => break,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(300)) => {
                        // [v0.10] 轮转自愈：len<pos（截断/换新）或**开头指纹变化**（copytruncate
                        // 后 len 可能恰好 == pos，len<pos 漏检）→ 重开从头读
                        let mut rotated = match std::fs::metadata(&path) {
                            Ok(m) => m.len() < pos,
                            Err(_) => true,
                        };
                        if !rotated {
                            let _ = file.seek(SeekFrom::Start(0));
                            let mut probe = vec![0u8; 64];
                            let n = file.read(&mut probe).unwrap_or(0);
                            probe.truncate(n);
                            // 只比共同前缀：文件增长（追加）不算轮转，开头内容变化才算
                            let common = n.min(start_fp.len());
                            rotated = probe[..common] != start_fp[..common];
                        }
                        if rotated {
                            if let Ok(f2) = std::fs::File::open(&path) {
                                file = f2;
                                pos = 0;
                                start_fp.clear();
                                let _ = file.read_to_end(&mut start_fp); // 新开头指纹
                                let _ = file.seek(SeekFrom::Start(pos));
                            }
                        }
                        let _ = file.seek(SeekFrom::Start(pos));
                        let mut grow = Vec::new();
                        let _ = file.read_to_end(&mut grow);
                        if !grow.is_empty() {
                            pos += grow.len() as u64;
                            let lines = lb.push(&grow);
                            if !lines.is_empty() {
                                let _ = tx.send(StreamEvent::Lines(lines)).await;
                            }
                        }
                    }
                }
            }
        });

        (
            Self {
                handle: None,
                stop_tx,
                rx,
            },
            jh,
        )
    }
}
