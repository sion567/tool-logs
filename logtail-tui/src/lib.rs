//! logtail-tui — 终端界面（§3.1）：状态机 / 主循环 / 渲染 / 键盘交互。
//!
//! 核心逻辑（SSH / 日志源 / 行解析过滤 / 配置）在 logtail-core；
//! 本 crate 只管 UI（ratatui / crossterm / arboard 依赖全部圈在这里）。

pub mod app;
pub mod ui;
