//! 日志数据模型与处理（§5）

pub mod buffer;
pub mod color;
pub mod filter;
pub mod parser;

pub use buffer::LogBuffer;
pub use color::colorize;
pub use filter::{
    active_filter_of, matches_filter, rebuild_view, routes_to_filter, text_patterns,
    edit_backspace, edit_insert, FilterMode, FilterState, LevelFilter, NO_FILTER,
};
pub use parser::{detect_level, parse_line};

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
    pub fn color(self) -> ratatui::style::Color {
        use ratatui::style::Color;
        match self {
            LogLevel::Error => Color::Red,
            LogLevel::Warn => Color::Yellow,
            LogLevel::Info => Color::Green,
            LogLevel::Debug => Color::Cyan,
            LogLevel::Unknown => Color::White,
        }
    }

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
