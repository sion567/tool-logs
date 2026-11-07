//! logtail-core — 连日志的核心库（§3.1 模块分层）
//!
//! 只含 UI 无关的部分：SSH 连接 / 日志源命令构造 / 行解析与过滤 / 配置。
//! 前端（logtail-tui / logtail-gui）只依赖本 crate 的公共 API，
//! 不得引用任何 ratatui / crossterm / egui 类型。

pub mod config;
pub mod error;
pub mod log;
pub mod source;
pub mod ssh;

/// 测试设施（in-process sshd 桩，§17）：仅 feature "test-util" 下编译，
/// 供本 crate 集成测试与前端 e2e 测试共用，常规构建不包含。
#[cfg(feature = "test-util")]
pub mod testing;
