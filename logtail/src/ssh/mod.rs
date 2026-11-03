//! SSH 模块（§7）：连接认证 / 命令流 / 会话泵

pub mod client;
pub mod session;
pub mod stream;

pub use client::{connect, ClientHandler};
pub use session::ActiveSession;
pub use stream::{LineBuffer, LogStream, StreamEvent};
