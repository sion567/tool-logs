//! 配置加载 / 保存 / 热加载（§8）；缺文件给模板（§四 Day5）

use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::Config;
use crate::error::LogtailError;

/// 首次运行时写入的模板（§8 样例 + remember_last_source）
pub const TEMPLATE: &str = r#"# ~/.config/logtail/config.toml — 首次运行自动生成，按需修改

[general]
default_tail = 500
max_buffer_lines = 10000
theme = "dark"
remember_last_source = true   # 退出记住激活源与模式，下次启动自动连

[[servers]]
name = "prod-01"
host = "192.168.1.100"
port = 22
user = "root"
auth = { type = "key", path = "~/.ssh/id_rsa" }        # ~ 由 expand_home 展开
# sudo = true   # 远端命令加 sudo -n 前缀（docker 归 root 时用；需配 sudoers NOPASSWD 白名单）

[[servers]]
name = "staging"
host = "192.168.1.101"
port = 22
user = "deploy"
auth = { type = "password", password = "change-me" }   # 具名字段

[[sources]]
name = "prod-api"
server = "prod-01"
source = { type = "docker", container = "my-app", tail = 500 }

[[sources]]
name = "prod-ffmpeg"
server = "prod-01"
source = { type = "file", path = "/var/log/ffmpeg/stream.log", tail = 200 }

[[sources]]
name = "host-system"
server = "prod-01"
source = { type = "journalctl", service = "systemd", since = "today" }

# [v0.8] Loki 源（远端需 curl+jq；一次性回放，按 L/f 换窗口）
# [[sources]]
# name = "loki-app"
# server = "prod-01"
# source = { type = "loki", url = "http://localhost:3100", query = '{job="my-app"}', limit = 1000 }

# [v0.8] 本地文件流（不经 SSH，server 可省略；tail 语义）
# [[sources]]
# name = "local-log"
# source = { type = "local", path = "C:/logs/app.log", tail = 200 }

[presets.quick_error]
name = "仅ERROR(1h)"
level = "error"
since = "1h"
"#;

pub fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

pub fn config_dir() -> PathBuf {
    home_dir().join(".config").join("logtail")
}

fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// 读 config.toml；缺文件 → 写模板并返回默认空配置
pub fn load() -> Result<Config> {
    let path = config_path();
    if !path.exists() {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&path, TEMPLATE)?;
        return Ok(Config::default());
    }
    let s = fs::read_to_string(&path)?;
    toml::from_str(&s).map_err(|e| {
        anyhow::Error::new(LogtailError::ConfigError(format!("{}: {e}", path.display())))
    })
}

/// 退出记住的状态（remember_last_source，[v0.4 三轮]）
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct StateFile {
    pub last_source: Option<String>,
    /// "full" | "quick" | "range"
    pub mode: Option<String>,
    pub since: Option<String>,
}

fn state_path() -> PathBuf {
    config_dir().join("state.toml")
}

/// [v0.7.1] 无条件追加错误日志：panic/启动失败/配置错误都落
/// `~/.config/logtail/logtail.log`——双击 exe 闪退时 stderr 一闪而过，这里永远有痕。
pub fn append_error_log(msg: &str) {
    let _ = fs::create_dir_all(config_dir());
    let line = format!("[{}] {}
", chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"), msg);
    let _ = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(config_dir().join("logtail.log"))
        .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
}

pub fn load_state() -> Option<StateFile> {
    let s = fs::read_to_string(state_path()).ok()?;
    toml::from_str(&s).ok()
}

pub fn save_state(state: &StateFile) {
    let _ = fs::create_dir_all(config_dir());
    if let Ok(text) = toml::to_string_pretty(state) {
        let _ = fs::write(state_path(), text);
    }
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]

    use super::*;
    use crate::config::AuthCfg;

    #[test]
    fn sudo_开关解析() {
        // [v0.7.7] server 级 sudo 字段：缺省 false，显式 true 生效
        let t = r#"
[[servers]]
name = "s"
host = "h"
port = 22
user = "u"
auth = { type = "password", password = "x" }
sudo = true
"#;
        let cfg: Config = toml::from_str(t).unwrap();
        assert!(cfg.servers[0].sudo);
        let cfg2: Config = toml::from_str(
            "[[servers]]
name='s'
host='h'
port=22
user='u'
auth={type=\"password\",password=\"x\"}",
        )
        .unwrap();
        assert!(!cfg2.servers[0].sudo, "缺省 false");
    }

    #[test]
    fn loki与local源解析() {
        // [v0.8] loki 源带 url/query/limit；local 源 server 可省略
        let t = r#"
[[sources]]
name = "lk"
server = "s"
source = { type = "loki", url = "http://localhost:3100", query = '{job="my-app"}', limit = 500 }

[[sources]]
name = "loc"
source = { type = "local", path = "/tmp/x.log", tail = 50 }
"#;
        let cfg: Config = toml::from_str(t).unwrap();
        assert_eq!(cfg.sources.len(), 2);
        assert_eq!(cfg.sources[0].server, "s");
        assert_eq!(cfg.sources[1].server, "", "本地源 server 缺省为空");
        match &cfg.sources[0].source {
            crate::source::LogSource::Loki { url, query, limit } => {
                assert_eq!(url, "http://localhost:3100");
                assert_eq!(query, "{job=\"my-app\"}");
                assert_eq!(*limit, 500);
            }
            other => panic!("应为 Loki，实际 {other:?}"),
        }
        assert!(matches!(
            cfg.sources[1].source,
            crate::source::LogSource::LocalFile { tail: 50, .. }
        ));
    }

    #[test]
    fn 模板自身可被解析() {
        let cfg: Config = toml::from_str(TEMPLATE).unwrap();
        assert_eq!(cfg.servers.len(), 2);
        assert_eq!(cfg.sources.len(), 3);
        assert_eq!(cfg.presets.len(), 1);
        assert!(cfg.general.remember_last_source);
    }

    #[test]
    fn authcfg_反序列化_具名字段() {
        // 内联表不能作 TOML 根文档，按真实配置形状包一层
        #[derive(Deserialize)]
        struct Wrap {
            auth: AuthCfg,
        }
        let w: Wrap = toml::from_str(r#"auth = { type = "password", password = "x" }"#).unwrap();
        assert!(matches!(w.auth, AuthCfg::Password { password } if password == "x"));
        let w: Wrap = toml::from_str(r#"auth = { type = "key", path = "~/.ssh/id_rsa" }"#).unwrap();
        assert!(matches!(w.auth, AuthCfg::Key { path, passphrase: None } if path == "~/.ssh/id_rsa"));
    }
}
