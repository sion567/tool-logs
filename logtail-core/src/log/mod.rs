//! 日志数据模型与处理（§5）

pub mod buffer;
pub mod color;
pub mod filter;
pub mod parser;

pub use buffer::{export_session, LogBuffer};
pub use color::colorize;
// 键盘层的 routes_to_filter / edit_insert / edit_backspace 属 TUI 输入处理，随 TUI 层走
pub use filter::{
    active_filter_of, matches_filter, rebuild_view, text_patterns, FilterMode, FilterState,
    LevelFilter, NO_FILTER,
};
pub use parser::{detect_level, parse_line, parse_line_multi};
use parser::strip_ts_prefix;

use std::sync::Arc;

use chrono::{DateTime, Utc};

/// 单行日志（§5.1）。raw 是已拆 docker JSON 包壳后的"可读行"，不含 ANSI。
///
/// [v0.4 P1#5] 必须 derive Default：parse_line 与测试都用 `..Default::default()`——
/// functional-update 的 `..` 后必须跟 base 表达式，`{ level: X, .. }` 是语法错误。
/// （DateTime<Utc>: Default 为 epoch，需 chrono ≥ 0.4.23。）
///
/// [v0.10.8] 内存瘦身：
/// - `source` 改 `Arc<str>`——同一会话/tab 的行共享同一份源名，逐行 clone 降为引用计数；
/// - `body` 不再存副本——`strip_ts_prefix` 的结果恒为 raw 的后缀切片，只记 (start,end)
///   字节区间，`body()` 零拷贝引用 raw（每行省一份消息体内容 + String 头）；
/// - 删掉从未写入的 `fields: HashMap`（每行白占 48B）。
#[derive(Debug, Clone, Default)]
pub struct LogLine {
    pub raw: String,
    pub level: LogLevel,
    pub timestamp: Option<DateTime<Utc>>,
    /// 逻辑源名，如 "prod-01/my-app"（[v0.10.8] Arc 共享，测试 `"x".into()` 照常可用）
    pub source: Arc<str>,
    /// [v0.10] 消息体在 raw 中的字节区间 [start, end)——raw 剥掉行首时间戳 token
    /// 后的剩余（相似行折叠的去重键）。默认 (0,0)=空；canonical 写入走
    /// `refresh_body`。pub 是为了 `..Default::default()` 字面量构造（本结构体
    /// 全 pub 字段风格）；[v0.10.8] raw 改动后须 refresh_body，`body()` 读取时
    /// 钳制到 raw 边界（区间过期不 panic）。
    pub body_range: (u32, u32),
    pub trace_id: Option<String>,
    /// 单调递增，全局唯一身份（§5.1 不变式①：UI/过滤/详情只认它）
    pub line_no: u64,
}

impl LogLine {
    /// [v0.10] 消息体：raw 剥掉行首时间戳后的剩余（零拷贝引用 raw）。
    /// 区间钳制到 raw 边界——raw 被改而未 refresh_body 时返回截断值而非 panic
    pub fn body(&self) -> &str {
        let s = (self.body_range.0 as usize).min(self.raw.len());
        let e = (self.body_range.1 as usize).min(self.raw.len()).max(s);
        &self.raw[s..e]
    }

    /// [v0.10.8] raw 定稿后调用：按后缀偏移重算 body 区间。
    /// 前提：raw 此后不再被修改（parser 两处赋值点与测试造行均满足）。
    pub fn refresh_body(&mut self) {
        let body = strip_ts_prefix(&self.raw);
        let start = self.raw.len() - body.len();
        self.body_range = (start as u32, self.raw.len() as u32);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum LogLevel {
    /// [v0.4 P1#5] Default 即 Unknown
    #[default]
    Unknown = 0,
    Debug = 1,
    Info = 2,
    Warn = 3,
    Error = 4,
}

impl LogLevel {
    // 级别颜色映射属 UI 层：TUI 见 logtail-tui/ui/render.rs 的 level_color()，
    // GUI 有自己的 egui 色表——core 不依赖任何渲染库
    pub fn label(self) -> &'static str {
        match self {
            LogLevel::Error => "ERROR",
            LogLevel::Warn => "WARN ",
            LogLevel::Info => "INFO ",
            LogLevel::Debug => "DEBUG",
            LogLevel::Unknown => "     ",
        }
    }
}

/// [v0.7] 时间轴 sparkline（core 化：TUI 工具栏与 GUI 通用）：
/// 以最新带时间戳的行对齐末端，按 bucket_secs 分桶计行数。
/// 距末端超过 width 个桶的旧行丢弃；无时间戳的行不计入。
pub fn sparkline(buf: &LogBuffer, width: usize, bucket_secs: i64) -> Vec<u64> {
    let mut end = None;
    for l in buf.iter() {
        if let Some(ts) = l.timestamp {
            end = Some(match end {
                Some(e) if e >= ts => e,
                _ => ts,
            });
        }
    }
    let mut v = vec![0u64; width];
    let Some(end) = end else { return v };
    for l in buf.iter() {
        if let Some(ts) = l.timestamp {
            let from_end = (end - ts).num_seconds().max(0) / bucket_secs;
            if let Some(idx) = width.checked_sub(1 + from_end as usize) {
                v[idx] += 1;
            }
        }
    }
    v
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]

    use super::*;
    use chrono::Duration;

    fn buf_with_ts(counts: &[(i64, u64)]) -> LogBuffer {
        let mut b = LogBuffer::new(100);
        let now = Utc::now();
        for (ago, n) in counts {
            for _ in 0..*n {
                let mut l = LogLine {
                    raw: "x".into(),
                    ..Default::default()
                };
                l.timestamp = Some(now - Duration::seconds(*ago));
                b.push(l);
            }
        }
        b
    }

    #[test]
    fn sparkline_按秒分桶_末端对齐最新() {
        let b = buf_with_ts(&[(0, 3), (2, 5), (10, 1)]);
        let v = sparkline(&b, 5, 1); // 覆盖 [now-5s, now]
        assert_eq!(v[4], 3, "最新一秒 3 行");
        assert_eq!(v[2], 5, "now-2s 桶 5 行");
        assert_eq!(v[0], 0, "now-5s 桶为空");
        // 无时间戳的 buffer → 全零
        let mut b2 = LogBuffer::new(10);
        b2.push(LogLine {
            raw: "no-ts".into(),
            ..Default::default()
        });
        assert!(sparkline(&b2, 5, 1).iter().all(|&c| c == 0));
    }
}
