//! 连接与认证（§7.1）

use anyhow::Result;
use russh::client::{self, Handle};

use crate::config::server::AuthCfg;
use crate::error::LogtailError;

/// [v0.10 §16.5] 主机密钥 TOFU：check_server_key 只记录指纹，
/// 接受/拒绝的判定在 connect_with 里与 known_hosts 比对后统一处理
#[derive(Debug)]
pub struct ClientHandler {
    pub found: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

#[async_trait::async_trait]
impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::key::PublicKey,
    ) -> Result<bool, Self::Error> {
        *self.found.lock().unwrap() = Some(server_public_key.fingerprint());
        Ok(true) // 判定延后：connect_with 比对 known_hosts
    }
}

/// 建立连接并完成认证（TOFU 主机密钥，known_hosts 用默认路径）
pub async fn connect(
    host: &str,
    port: u16,
    user: &str,
    auth: &AuthCfg,
) -> Result<Handle<ClientHandler>> {
    connect_with(host, port, user, auth, &known_hosts_path()).await
}

/// [v0.10 §16.5] TOFU 已知主机文件路径
pub fn known_hosts_path() -> std::path::PathBuf {
    crate::config::config_dir().join("known_hosts")
}

/// 已知主机条目：`host:port fingerprint`
fn read_known_host(path: &std::path::Path, ident: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()?
        .lines()
        .find_map(|l| {
            let (id, fp) = l.split_once(' ')?;
            (id == ident).then(|| fp.to_string())
        })
}

fn append_known_host(path: &std::path::Path, ident: &str, fp: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{ident} {fp}")?;
    Ok(())
}

/// 建立连接并完成认证 + [v0.10 §16.5] 主机密钥 TOFU 指纹固定：
/// - 首次连接（known_hosts 无条目）：记录指纹并继续
/// - 已有条目且指纹一致：继续
/// - 已有条目且指纹不一致：**拒绝连接**（疑似 MITM），报错指明处理办法
pub async fn connect_with(
    host: &str,
    port: u16,
    user: &str,
    auth: &AuthCfg,
    known_hosts: &std::path::Path,
) -> Result<Handle<ClientHandler>> {
    use std::sync::{Arc, Mutex};
    let ident = format!("{host}:{port}");
    let expected = read_known_host(known_hosts, &ident);
    let found = Arc::new(Mutex::new(None::<String>));

    let config = Arc::new(client::Config::default());
    let mut handle = client::connect(config, (host, port), ClientHandler { found: found.clone() })
        .await
        .map_err(|e| LogtailError::SshConnect(format!("{host}:{port}: {e}")))?;

    let fp = found
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| LogtailError::SshConnect("未能取得主机密钥指纹".into()))?;
    match &expected {
        Some(e) if *e != fp => {
            return Err(LogtailError::SshConnect(format!(
                "主机密钥指纹不匹配（{ident}）：记录={e}，本次={fp}。若确认服务器重装/换钥，删除 known_hosts 中该行后重试"
            ))
            .into());
        }
        Some(_) => {}
        None => append_known_host(known_hosts, &ident, &fp)?, // TOFU 首连记录
    }
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
