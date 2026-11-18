//! 服务器与认证配置（§7.1、§8）

use serde::{Deserialize, Serialize};

use super::keyring;
use crate::error::LogtailError;

/// [v0.3 P1#5] 具名字段 + serde 内部 tag，才能被
/// `auth = { type = "password", password = "x" }` 这类 toml 内联表反序列化。
/// rename_all = lowercase：config 里写的是小写 `type = "password"|"key"|"keyring"`。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum AuthCfg {
    Password {
        password: String,
    },
    Key {
        path: String,
        passphrase: Option<String>,
    },
    /// [v0.10.9] 密钥环认证（§16.3）：密码存 OS 凭据库，config.toml 不落明文。
    /// 账户名缺省 = 服务器 name；先 `logtail-tui keyring set <名>` 写入再切换到此认证
    Keyring {
        account: Option<String>,
    },
}

impl AuthCfg {
    /// `--check-config` / 状态展示用：认证方式一句话
    pub fn label(&self) -> String {
        match self {
            AuthCfg::Password { .. } => "密码认证（明文；建议迁移密钥环）".into(),
            AuthCfg::Key { .. } => "密钥认证".into(),
            AuthCfg::Keyring { account: Some(a) } => format!("密钥环认证（账户 {a}）"),
            AuthCfg::Keyring { account: None } => "密钥环认证".into(),
        }
    }
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

impl ServerConfig {
    /// Keyring 认证的密钥环账户名：显式 `account` 优先，缺省 = 服务器 name
    fn keyring_account(&self) -> String {
        match &self.auth {
            AuthCfg::Keyring { account: Some(a) } => a.clone(),
            _ => self.name.clone(),
        }
    }

    /// [v0.10.9] 登录认证解析：Keyring → 从系统密钥环取密码转成 Password
    /// （其余认证原样克隆）。TUI/GUI 连接前各调一次——此后链路只见 Password，
    /// client.rs 的 russh 认证零改动；GUI 侧解析发生在 pump 线程，前端内存不碰密钥。
    pub fn resolved_auth(&self) -> Result<AuthCfg, LogtailError> {
        match &self.auth {
            AuthCfg::Keyring { .. } => Ok(AuthCfg::Password {
                password: keyring::get(&self.keyring_account())?,
            }),
            other => Ok(other.clone()),
        }
    }

    /// [v0.10.9] sudo 密码解析（收口 TUI do_connect 与 GUI remote 两份重复逻辑）：
    /// 未开 sudo → None；显式 sudo_password（非空，空串视为未设——与既有文档一致）
    /// 优先；否则复用解析后的登录密码（sudo 密码=登录密码惯例，Keyring 同样走密钥环）；
    /// 密钥/无密码 → None（走 `sudo -n`，NOPASSWD 场景）。
    pub fn resolved_sudo_password(&self) -> Result<Option<String>, LogtailError> {
        if !self.sudo {
            return Ok(None);
        }
        if let Some(pw) = self.sudo_password.as_deref().filter(|s| !s.is_empty()) {
            return Ok(Some(pw.to_string()));
        }
        match self.resolved_auth()? {
            AuthCfg::Password { password } if !password.is_empty() => Ok(Some(password)),
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]

    use super::*;
    use crate::config::keyring;

    fn server(name: &str, auth: AuthCfg) -> ServerConfig {
        ServerConfig {
            name: name.into(),
            host: "h".into(),
            port: 22,
            user: "u".into(),
            auth,
            sudo: false,
            sudo_password: None,
        }
    }

    #[test]
    fn keyring认证_toml解析_缺省与显式账户() {
        // 内部 tag 内联表两种写法都能反序列化
        let s: ServerConfig = toml::from_str(
            r#"
name = "prod-01"
host = "1.2.3.4"
port = 22
user = "root"
auth = { type = "keyring" }
"#,
        )
        .unwrap();
        assert!(matches!(s.auth, AuthCfg::Keyring { account: None }));
        let s2: ServerConfig = toml::from_str(
            r#"
name = "prod-01"
host = "1.2.3.4"
port = 22
user = "root"
auth = { type = "keyring", account = "shared" }
"#,
        )
        .unwrap();
        assert_eq!(
            s2.auth.label(),
            "密钥环认证（账户 shared）",
            "显式账户进 label"
        );
    }

    #[test]
    fn resolved_auth_keyring_取密钥环转password_明文密钥原样() {
        // Keyring：密钥环里的密码转成 Password
        keyring::set("srv-kr", "kr-pw").unwrap();
        let s = server("srv-kr", AuthCfg::Keyring { account: None });
        assert_eq!(
            s.resolved_auth().unwrap(),
            AuthCfg::Password {
                password: "kr-pw".into()
            }
        );
        // 显式 account 覆盖服务器名
        keyring::set("shared-acct", "shared-pw").unwrap();
        let s2 = server(
            "srv-kr",
            AuthCfg::Keyring {
                account: Some("shared-acct".into()),
            },
        );
        assert_eq!(
            s2.resolved_auth().unwrap(),
            AuthCfg::Password {
                password: "shared-pw".into()
            }
        );
        // 缺条目 → 可操作错误
        let s3 = server("no-such-srv", AuthCfg::Keyring { account: None });
        let err = s3.resolved_auth().unwrap_err().to_string();
        assert!(
            err.contains("keyring set no-such-srv"),
            "缺条目报错带指引: {err}"
        );
        // Password / Key 原样克隆，不触密钥环
        let s4 = server(
            "plain",
            AuthCfg::Password {
                password: "p".into(),
            },
        );
        assert_eq!(
            s4.resolved_auth().unwrap(),
            AuthCfg::Password {
                password: "p".into()
            }
        );
        let s5 = server(
            "keyed",
            AuthCfg::Key {
                path: "~/.ssh/id_ed25519".into(),
                passphrase: None,
            },
        );
        assert!(matches!(s5.resolved_auth().unwrap(), AuthCfg::Key { .. }));
    }

    #[test]
    fn resolved_sudo_password_四条回退路径() {
        // ① 未开 sudo → None（Keyring 条目都不用碰）
        let no_sudo = server(
            "sudo-off",
            AuthCfg::Keyring { account: None }, // 故意不给条目，验证不触密钥环
        );
        assert_eq!(no_sudo.resolved_sudo_password().unwrap(), None);
        // ② 显式 sudo_password（非空）优先
        let mut explicit = server(
            "sudo-explicit",
            AuthCfg::Password {
                password: "login-pw".into(),
            },
        );
        explicit.sudo = true;
        explicit.sudo_password = Some("sudo-pw".into());
        assert_eq!(explicit.resolved_sudo_password().unwrap().as_deref(), Some("sudo-pw"));
        // ②b 显式空串 = 未设 → 回退登录密码（文档语义：空字符串视为未设置）
        explicit.sudo_password = Some(String::new());
        assert_eq!(
            explicit.resolved_sudo_password().unwrap().as_deref(),
            Some("login-pw")
        );
        // ③ Keyring 认证回退密钥环登录密码
        keyring::set("sudo-kr", "kr-login").unwrap();
        let mut kr = server("sudo-kr", AuthCfg::Keyring { account: None });
        kr.sudo = true;
        assert_eq!(
            kr.resolved_sudo_password().unwrap().as_deref(),
            Some("kr-login")
        );
        // ④ Key 认证无显式 sudo_password → None（sudo -n / NOPASSWD）
        let mut keyed = server(
            "sudo-key",
            AuthCfg::Key {
                path: "~/.ssh/id_ed25519".into(),
                passphrase: None,
            },
        );
        keyed.sudo = true;
        assert_eq!(keyed.resolved_sudo_password().unwrap(), None);
    }
}
