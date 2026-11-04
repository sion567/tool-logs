//! 服务器与认证配置（§7.1、§8）

use serde::{Deserialize, Serialize};

/// [v0.3 P1#5] 具名字段 + serde 内部 tag，才能被
/// `auth = { type = "password", password = "x" }` 这类 toml 内联表反序列化。
/// rename_all = lowercase：config 里写的是小写 `type = "password"|"key"`。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum AuthCfg {
    Password { password: String },
    Key { path: String, passphrase: Option<String> },
}

/// 服务器声明（§8）
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerConfig {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: AuthCfg,
    /// [v0.7.7] 远程命令加 `sudo -n` 前缀——适用于"docker 归 root、账号不在 docker 组"的
    /// 机器（§16 配套：服务器上配 sudoers NOPASSWD 白名单，最小权限，不给整组等效 root）。
    /// `-n` 非交互：未配 NOPASSWD 时 sudo 直接报错进日志流，不会卡住等待密码。
    #[serde(default)]
    pub sudo: bool,
}
