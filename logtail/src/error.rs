//! 错误处理（§15）
//!
//! UI 铁律：错误走状态栏 / 弹层，绝不 panic。
//! 会话级错误 → SessionState::Failed，允许按 R 重试，不拖整个程序。

use thiserror::Error;

#[derive(Debug, Error)]
pub enum LogtailError {
    #[error("SSH 连接失败: {0}")]
    SshConnect(String),
    #[error("认证失败: {0}")]
    AuthFailed(String),
    #[error("远程命令执行失败: {0}")]
    ExecFailed(String),
    #[error("配置加载失败: {0}")]
    ConfigError(String),
    #[error("正则编译错误: {0}")]
    RegexError(String),
    /// [v0.3 P1#3] 白名单拒绝，不做"转义放行"
    #[error("不安全输入，已拒绝执行: {0}")]
    UnsafeInput(String),
}
