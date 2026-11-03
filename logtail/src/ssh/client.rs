//! 连接与认证（§7.1）

use std::sync::Arc;

use anyhow::Result;
use russh::client::{self, Handle};

use crate::config::server::AuthCfg;
use crate::error::LogtailError;

#[derive(Debug)]
pub struct ClientHandler;

#[async_trait::async_trait]
impl client::Handler for ClientHandler {
    type Error = russh::Error;

    /// [§16.5] 开发态盲信首见主机密钥；上线前改指纹固定列表 / known_hosts 校验
    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::key::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

/// 建立连接并完成认证。认证被拒绝 / 密钥加载失败都显式报错，不留半开连接。
pub async fn connect(
    host: &str,
    port: u16,
    user: &str,
    auth: &AuthCfg,
) -> Result<Handle<ClientHandler>> {
    let config = Arc::new(client::Config::default());
    let mut handle = client::connect(config, (host, port), ClientHandler)
        .await
        .map_err(|e| LogtailError::SshConnect(format!("{host}:{port}: {e}")))?;

    let ok = match auth {
        AuthCfg::Password { password } => handle
            .authenticate_password(user, password.as_str())
            .await
            .map_err(|e| LogtailError::AuthFailed(format!("{user}@{host}: {e}")))?,
        AuthCfg::Key { path, passphrase } => {
            let key = russh::keys::load_secret_key(expand_home(path), passphrase.as_deref())
                .map_err(|e| LogtailError::AuthFailed(format!("加载私钥 {path} 失败: {e}")))?;
            handle
                .authenticate_publickey(user, Arc::new(key))
                .await
                .map_err(|e| LogtailError::AuthFailed(format!("{user}@{host}: {e}")))?
        }
    };
    if !ok {
        return Err(LogtailError::AuthFailed(format!("{user}@{host} 认证被拒绝")).into());
    }
    Ok(handle)
}

/// [§16.4] 只认 ~/ 前缀；HOME 缺失则原样返回，让连接阶段暴露明确错误
fn expand_home(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
            return format!("{}/{}", home.trim_end_matches('/'), rest);
        }
    }
    p.to_string()
}
