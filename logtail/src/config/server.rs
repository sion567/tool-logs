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
    /// [v0.7.7] 远程命令加 sudo 前缀——适用于"docker 归 root、账号不在 docker 组"的机器。
    /// 密码模式二选一（v0.8.1）：
    /// - 配 sudoers NOPASSWD 白名单 → 走 `sudo -n`（无需密码）
    /// - 未配 NOPASSWD（习惯 sudo 输密码）→ 走 `sudo -S`：sudo_password 未填时
    ///   自动复用 auth.password（sudo 密码=登录密码的常见场景），密码经 SSH stdin 写入
    #[serde(default)]
    pub sudo: bool,
    /// [v0.8.1] sudo 密码；缺省复用 auth.password（Password 认证时）
    #[serde(default)]
    pub sudo_password: Option<String>,
}
