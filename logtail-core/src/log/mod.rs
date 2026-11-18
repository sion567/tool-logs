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

use std::collections::HashMap;

use chrono::{DateTime, Utc};

/// 单行日志（§5.1）。raw 是已拆 docker JSON 包壳后的"可读行"，不含 ANSI。
///
/// [v0.4 P1#5] 必须 derive Default：parse_line 与测试都用 `..Default::default()`——
/// functional-update 的 `..` 后必须跟 base 表达式，`{ level: X, .. }` 是语法错误。
/// （DateTime<Utc>: Default 为 epoch，需 chrono ≥ 0.4.23。）
#[derive(Debug, Clone, Default)]
pub struct LogLine {
    pub raw: String,
    pub level: LogLevel,
    pub timestamp: Option<DateTime<Utc>>,
    /// 逻辑源名，如 "prod-01/my-app"
    pub source: String,
    /// [v0.10] 消息体：raw 剥掉行首时间戳 token 后的剩余（相似行折叠的去重键）
    pub body: String,
    pub trace_id: Option<String>,
    pub fields: HashMap<String, String>,
    /// 单调递增，全局唯一身份（§5.1 不变式①：UI/过滤/详情只认它）
    pub line_no: u64,
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
