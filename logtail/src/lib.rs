//! logtail — 远程日志查看工具（§3.1 模块分层）
//!
//! lib.rs 使模块可被 tests/integration.rs 引用（§17 集成测试）；
//! main.rs 只保留入口与终端生命周期。

pub mod app;
pub mod config;
pub mod error;
pub mod log;
pub mod source;
pub mod ssh;
pub mod ui;
