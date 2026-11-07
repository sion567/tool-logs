//! 级别 → ANSI（Day2 无头打印用；TUI 走 ratatui Style，见 render.rs）

use crate::log::LogLine;
use crate::log::LogLevel;

impl LogLevel {
    /// 终端 ANSI 前缀（§四 Day2：31 红 / 33 黄 / 32 绿 / 36 青）
    pub fn ansi(self) -> &'static str {
        match self {
            LogLevel::Error => "\x1b[31m",
            LogLevel::Warn => "\x1b[33m",
            LogLevel::Info => "\x1b[32m",
            LogLevel::Debug => "\x1b[36m",
            LogLevel::Unknown => "\x1b[37m",
        }
    }
}

pub const RESET: &str = "\x1b[0m";

/// 无头模式的一行着色（不带时间戳渲染，TUI 里另行排版）
pub fn colorize(line: &LogLine) -> String {
    format!("{}{}{}", line.level.ansi(), line.raw, RESET)
}
