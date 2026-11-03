//! 日志源抽象（§6）；Day5 补 LogSource / to_command / 安全校验 / ModeVariant

/// 解析器用：是否拆 docker JSON 包壳（§6.1 [v0.4 P2#6]）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceType {
    Docker,
    File,
    Journalctl,
    Command,
}
