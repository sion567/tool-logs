//! LogBuffer（§5.2）：line_no 连续窗口 + front 偏移反查，淘汰零维护。
//!
//! [v0.4 P0#1] v0.3 的 HashMap<u64, usize> 反查在 pop_front 淘汰后整表错位
//! （只 remove 了被淘汰项，剩余项 value 没整体前移），by_line_no 返回错行。
//! 既然 line_no 连续、lines 永远是连续窗口，`i = no - front_no` 就是 O(1) 定位。

use std::collections::VecDeque;

use crate::log::LogLine;

pub struct LogBuffer {
    lines: VecDeque<LogLine>,
    max: usize,
    next_no: u64,
}

impl LogBuffer {
    pub fn new(max: usize) -> Self {
        // max=0 防御：否则 push 时 len(0)>=0 触发空 pop 后仍入栈，窗口永不淘汰；
        // 视图兜底的 EMPTY buffer 用 new(0)，按容量 1 语义收敛
        let max = max.max(1);
        Self {
            lines: VecDeque::with_capacity(max.min(1024)),
            max,
            next_no: 0,
        }
    }

    /// 窗口内最小 line_no；空 buffer 取 next_no（此时任何查询都应落空）
    /// [v0.10.8] pub：GUI 增量视图按它识别被淘汰的 view 头部项
    pub fn front_no(&self) -> u64 {
        self.lines
            .front()
            .map(|l| l.line_no)
            .unwrap_or(self.next_no)
    }

    /// [v0.10.8] 下一个待分配 line_no（增量视图的水位基准）
    pub fn next_no(&self) -> u64 {
        self.next_no
    }

    /// [v0.10.8] 运行中调整容量（GUI 设置菜单改行数上限）：改阈值并立即截断
    /// 超容量部分（淘汰最旧——push 每次只弹一行，只改阈值的话大窗口永远缩不下去）。
    /// 视图层下次 ensure_view 按前移后的 front_no 修复头部项。
    pub fn set_max(&mut self, max: usize) {
        self.max = max.max(1);
        while self.lines.len() > self.max {
            self.lines.pop_front();
        }
    }

    /// 返回赋给该行的 line_no（调用方可用于选中/命中缓存）
    pub fn push(&mut self, mut line: LogLine) -> u64 {
        line.line_no = self.next_no;
        let no = line.line_no;
        if self.lines.len() >= self.max {
            // no 不回收：窗口整体前移，旧 no 查询天然落空——零维护
            self.lines.pop_front();
        }
        self.lines.push_back(line);
        self.next_no += 1;
        no
    }

    /// O(1) 反查：no < front_no 即已淘汰；i = no - front_no 越界即非法 no
    pub fn by_line_no(&self, no: u64) -> Option<&LogLine> {
        let front = self.front_no();
        if no < front {
            return None;
        }
        let i = (no - front) as usize;
        if i >= self.lines.len() {
            return None;
        }
        self.lines.get(i)
    }

    /// 上下文窗口（Day6 详情面板用）：no±k 的连续切片
    pub fn context(&self, no: u64, before: usize, after: usize) -> Vec<&LogLine> {
        let start = no.saturating_sub(before as u64);
        (start..=no + after as u64)
            .filter_map(|n| self.by_line_no(n))
            .collect()
    }

    pub fn iter(&self) -> impl Iterator<Item = &LogLine> {
        self.lines.iter()
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// 渲染期：从末尾取 visible 行。
    /// [v0.4 P2#1] VecDeque 底层分片不保证连续，`&self.lines[start..]` 拿不到 `[LogLine]`；
    /// 返回迭代器（调用方确需切片时先 make_contiguous）。
    pub fn tail(&self, visible: usize) -> impl Iterator<Item = &LogLine> {
        let start = self.lines.len().saturating_sub(visible);
        self.lines.iter().skip(start)
    }
}

/// [v0.7] 会话导出（core 化：TUI `e` 键与 GUI 导出共用）：`时间戳 级别 原始行`，
/// 头部带源名与行数（§19：会话级 .log，不做长期存储）。
pub fn export_session(
    buf: &LogBuffer,
    sid: &str,
    dir: &std::path::Path,
) -> anyhow::Result<std::path::PathBuf> {
    use std::fmt::Write as _;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let path = dir.join(format!("logtail-{sid}-{stamp}.log"));
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# logtail 会话导出 源={sid} 行数={} 时间={}",
        buf.len(),
        chrono::Local::now().to_rfc3339()
    );
    for l in buf.iter() {
        let ts = l
            .timestamp
            .map(|t| t.to_rfc3339())
            .unwrap_or_else(|| "-".into());
        let _ = writeln!(out, "{} {:>5} {}", ts, l.level.label().trim(), l.raw);
    }
    std::fs::write(&path, out)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_淘汰后_by_line_no_仍正确() {
        let mut b = LogBuffer::new(2);
        let no_a = b.push(LogLine {
            raw: "a".into(),
            ..Default::default()
        }); // no=0
        let no_b = b.push(LogLine {
            raw: "b".into(),
            ..Default::default()
        }); // no=1
        let no_c = b.push(LogLine {
            raw: "c".into(),
            ..Default::default()
        }); // no=2，淘汰 a
        assert!(b.by_line_no(no_a).is_none(), "已淘汰 → None");
        assert_eq!(b.by_line_no(no_b).unwrap().raw, "b", "淘汰后其余 no 不漂移");
        assert_eq!(b.by_line_no(no_c).unwrap().raw, "c");
        assert_eq!(b.len(), 2);
    }

    #[test]
    fn new_0_按容量1收敛() {
        let mut b = LogBuffer::new(0);
        let no0 = b.push(LogLine {
            raw: "a".into(),
            ..Default::default()
        });
        let no1 = b.push(LogLine {
            raw: "b".into(),
            ..Default::default()
        });
        assert_eq!(b.len(), 1, "容量 1：只留最新");
        assert!(b.by_line_no(no0).is_none());
        assert!(b.by_line_no(no1).is_some());
    }

    #[test]
    fn set_max_缩容立即截断并保留最新() {
        let mut b = LogBuffer::new(4);
        for s in ["l0", "l1", "l2", "l3"] {
            b.push(LogLine {
                raw: s.into(),
                ..Default::default()
            });
        }
        b.set_max(2); // 立即截断最旧 2 行，保留最新
        assert_eq!(b.len(), 2);
        assert!(b.by_line_no(0).is_none() && b.by_line_no(1).is_none());
        assert_eq!(b.by_line_no(2).unwrap().raw, "l2");
        // 后续 push 按新容量正常淘汰
        let no4 = b.push(LogLine {
            raw: "l4".into(),
            ..Default::default()
        });
        assert_eq!(b.len(), 2, "容量 2：push 一行淘汰一行");
        assert!(b.by_line_no(2).is_none());
        assert_eq!(b.by_line_no(no4).unwrap().raw, "l4");
        b.set_max(0); // 防御：按容量 1 收敛
        assert_eq!(b.len(), 1);
        b.push(LogLine {
            raw: "l5".into(),
            ..Default::default()
        });
        assert_eq!(b.len(), 1);
        // [v0.10.8] 水位访问器：增量视图基准
        assert_eq!(b.next_no(), 6);
        assert_eq!(b.front_no(), 5);
    }

    #[test]
    fn context_窗口跨越淘汰边界() {
        let mut b = LogBuffer::new(4);
        for s in ["l0", "l1", "l2", "l3", "l4", "l5"] {
            b.push(LogLine {
                raw: s.into(),
                ..Default::default()
            });
        }
        // 窗口现剩 l2..l5（no=2..5），no=4 上下文 ±2 → l2,l3,l4,l5
        let got: Vec<&str> = b.context(4, 2, 2).iter().map(|l| l.raw.as_str()).collect();
        assert_eq!(got, vec!["l2", "l3", "l4", "l5"]);
    }
}
