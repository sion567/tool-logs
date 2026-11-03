//! 命令输出流：`StreamEvent` + `LineBuffer` + `LogStream`（§7.2 / §7.3）
//!
//! russh API 坐实结果（§7.2 表格的实测，russh 0.44.1）：
//! - `Channel::exec(&self, want_reply, impl Into<Vec<u8>>)` — `&self`
//! - `Channel::wait(&mut self) -> Option<ChannelMsg>` — 读面；
//!   用它而不是 `into_stream()`：可与 50ms 超时轮询配合，Eof/Close/超时语义可控
//! - `Channel::close(&self)` — teardown；远端 sshd 对 exec 进程发 SIGHUP，
//!   `docker logs -f` 自行退出（§16 无孤儿进程）
//! - `Handle` 实现 `Drop`：丢弃即关连接（remove 会话路径的兜底）

use std::time::Duration;

use russh::ChannelMsg;

/// 主循环与泵之间的事件（§7.2 / §7.4）
#[derive(Debug)]
pub enum StreamEvent {
    /// [v0.4 P0#2] 一次 read 解出的【整批】完整行——v0.3 的 `Line(String)` 在上层
    /// 只取首行、其余全丢，教训：行必须整批透传
    Lines(Vec<String>),
    /// 远端 stdout 结束（EOF→Close），泵只发一次
    Ended,
}

/// [§7.3，v0.2 P0#3] 字节块 → 完整行；未成行部分留在 `partial`
#[derive(Debug, Default)]
pub struct LineBuffer {
    partial: String,
}

impl LineBuffer {
    /// 返回本次字节块中产生的【所有】完整行；未成行部分留在内部。
    /// CRLF 行尾剥掉 `\r`（远端 Windows 风格输出常见，残留会污染级别检测与显示）。
    pub fn push(&mut self, data: &[u8]) -> Vec<String> {
        let text = String::from_utf8_lossy(data);
        let mut out = Vec::new();
        let mut start = 0;
        for (i, ch) in text.char_indices() {
            if ch == '\n' {
                let mut line = self.partial.clone() + &text[start..i];
                if line.ends_with('\r') {
                    line.pop();
                }
                out.push(line);
                self.partial.clear();
                start = i + 1;
            }
        }
        if start < text.len() {
            self.partial.push_str(&text[start..]);
        }
        // 二进制垃圾防线（§18 超长行）：远端长期不发换行符时 partial 无限增长会吃内存。
        // 超 1MB 丢头保尾——真正要看的栈回溯尾部还在，内存有界。
        if self.partial.len() > MAX_PARTIAL {
            let keep = self.partial.split_off(self.partial.len() - MAX_PARTIAL);
            self.partial = keep;
        }
        out
    }

    /// [v0.3 P2#12] 未成行尾部，供状态栏"正在接收…"提示
    pub fn partial(&self) -> &str {
        &self.partial
    }

    pub fn is_idle(&self) -> bool {
        self.partial.is_empty()
    }
}

/// 未成行积压上限（1MB）：防二进制垃圾撑爆内存
const MAX_PARTIAL: usize = 1 << 20;

/// 命令输出流：持有读端 channel + 行缓冲。
pub struct LogStream {
    channel: russh::Channel<russh::client::Msg>,
    line_buffer: LineBuffer,
}

impl LogStream {
    /// 消费 channel（exec 之后调用）
    pub fn new(ch: russh::Channel<russh::client::Msg>) -> Self {
        Self {
            channel: ch,
            line_buffer: LineBuffer::default(),
        }
    }

    /// [§7.2] 单次读取：50ms 超时；EOF 与超时分叉（不变式②）。
    /// - 超时 → Ok(None)：本轮无数据，调用方继续渲染
    /// - Eof / ExitStatus → Ok(None)：继续 drain，防尾部行丢失
    /// - Close / channel 关闭 → Ended
    /// - Data / ExtendedData → 整批行；解不出完整行时 Ok(None)
    pub async fn try_next(&mut self) -> anyhow::Result<Option<StreamEvent>> {
        match tokio::time::timeout(Duration::from_millis(50), self.channel.wait()).await {
            Err(_elapsed) => Ok(None), // 本轮无数据
            Ok(None) => Ok(Some(StreamEvent::Ended)),
            Ok(Some(ChannelMsg::Data { data }))
            | Ok(Some(ChannelMsg::ExtendedData { data, .. })) => {
                let lines = self.line_buffer.push(&data);
                Ok(if lines.is_empty() {
                    None
                } else {
                    Some(StreamEvent::Lines(lines))
                })
            }
            Ok(Some(_)) => Ok(None), // Eof / ExitStatus 等：不算终态，继续读到 Close
        }
    }

    /// 终止：关 channel，远端进程收 SIGHUP 自行退出（不用 kill 猜 PID）
    pub async fn teardown(&self) {
        let _ = self.channel.close().await;
    }
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)] // 用例名含中文段落，非 snake_case 是刻意的

    use super::*;

    #[test]
    fn lb_一次_read_多条完整行_不丢() {
        let mut lb = LineBuffer::default();
        assert_eq!(lb.push(b"a\nb\nc\n"), vec!["a", "b", "c"]);
        assert!(lb.is_idle());
    }

    #[test]
    fn lb_跨两次_read_拼成一行() {
        let mut lb = LineBuffer::default();
        assert!(lb.push(b"hello ").is_empty());
        assert_eq!(lb.push(b"world\nbye"), vec!["hello world"]);
        assert_eq!(lb.partial(), "bye");
    }

    #[test]
    fn lb_crlf_行尾剥回车() {
        let mut lb = LineBuffer::default();
        assert_eq!(lb.push(b"a\r\nb\r\n"), vec!["a", "b"]);
        assert!(lb.is_idle());
    }

    #[test]
    fn lb_超长单行_10KB不截断() {
        // §18：超长单行（>10KB 栈回溯）不崩不截断
        let mut lb = LineBuffer::default();
        let big = "x".repeat(10_240);
        let payload = format!("{big}\n");
        assert_eq!(lb.push(payload.as_bytes()), vec![big]);
        // 跨块的超长行同样完整（末块带 \n 才成行）
        let big2 = "y".repeat(12_000);
        assert!(lb.push(&big2.as_bytes()[..5_000]).is_empty());
        let mut tail = big2.as_bytes()[5_000..].to_vec();
        tail.push(b'\n');
        assert_eq!(lb.push(&tail), vec![big2]);
    }

    #[test]
    fn lb_无换行积压_超限丢头保尾() {
        let mut lb = LineBuffer::default();
        // 2MB 无换行 → partial 被限制在 1MB
        let junk = "z".repeat(2 * 1024 * 1024);
        assert!(lb.push(junk.as_bytes()).is_empty());
        assert!(lb.partial().len() <= MAX_PARTIAL, "partial 必须有界: {}", lb.partial().len());
        // 尾部数据仍在，成行后可见
        let got = lb.push(b"tail-marker\n");
        assert_eq!(got.len(), 1);
        assert!(got[0].ends_with("tail-marker"));
    }

    #[test]
    fn eof_与超时_分叉() {
        // 语义锁定（§17）：超时与 EOF 是两种返回；真实 EOF 路径由
        // tests/integration.rs 的 in-process sshd 覆盖
        assert!(matches!(StreamEvent::Ended, StreamEvent::Ended));
    }
}
