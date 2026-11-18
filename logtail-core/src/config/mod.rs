//! 配置格式（§8）

pub mod keyring;
pub mod load;
pub mod server;
pub mod source_cfg;

use std::collections::BTreeMap;

use serde::Deserialize;

pub use load::{append_log, config_dir, load, load_state, save_state, StateFile};
pub use server::{AuthCfg, ServerConfig};
pub use source_cfg::SourceConfig;

/// [v0.9] UI 显示设置（§12 可配置化）：config `[ui]` 节为默认值，
/// 运行时热键切换后经 state.toml 持久化（下次启动沿用）
#[derive(Debug, Clone, Deserialize)]
pub struct UiCfg {
    #[serde(default = "d_true")]
    pub show_sparkline: bool,
    #[serde(default = "d_true")]
    pub show_sidebar: bool,
    /// 状态栏键位提示（隐藏后腾出空间给消息）
    #[serde(default = "d_true")]
    pub show_hints: bool,
    /// [v0.10] 相似行折叠 ×N
    #[serde(default)]
    pub dedupe: Option<bool>,
    /// [v0.10.8] 倒序显示（最新在顶）；None/false = 时间正序（默认）
    #[serde(default)]
    pub reverse: Option<bool>,
}

impl Default for UiCfg {
    fn default() -> Self {
        Self {
            show_sparkline: true,
            show_sidebar: true,
            show_hints: true,
            dedupe: None,
            reverse: None,
        }
    }
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub general: GeneralCfg,
    #[serde(default)]
    pub servers: Vec<ServerConfig>,
    #[serde(default)]
    pub sources: Vec<SourceConfig>,
    /// BTreeMap 保持文件顺序无关的稳定遍历（侧栏预设展示顺序稳定）
    #[serde(default)]
    pub presets: BTreeMap<String, PresetCfg>,
    /// [v0.9] UI 显示设置
    #[serde(default)]
    pub ui: UiCfg,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GeneralCfg {
    #[serde(default = "d_default_tail")]
    pub default_tail: usize,
    #[serde(default = "d_max_buffer_lines")]
    pub max_buffer_lines: usize,
    #[serde(default = "d_theme")]
    pub theme: String,
    /// [v0.4 三轮] 退出记住激活源与模式，下次启动自动连
    #[serde(default = "d_true")]
    pub remember_last_source: bool,
}

impl Default for GeneralCfg {
    fn default() -> Self {
        Self {
            default_tail: d_default_tail(),
            max_buffer_lines: d_max_buffer_lines(),
            theme: d_theme(),
            remember_last_source: d_true(),
        }
    }
}

fn d_default_tail() -> usize {
    500
}
fn d_max_buffer_lines() -> usize {
    10_000
}
fn d_theme() -> String {
    "dark".into()
}
fn d_true() -> bool {
    true
}

/// [presets.<key>] 表（§8 / §10）
#[derive(Debug, Clone, Deserialize)]
pub struct PresetCfg {
    pub name: Option<String>,
    /// "error"/"warn"/"info"/"debug"
    pub level: Option<String>,
    /// docker 源直接用；journalctl 源在 to_command 里换算绝对时间
    pub since: Option<String>,
}

impl PresetCfg {
    /// [v0.10 core 化] 转运行时预设（原 TUI app.rs 的 parse_level 收编，两前端共用）：
    /// level 字符串→LevelFilter（非法值按 None 处理），since 原样透传。
    pub fn to_preset(&self, key: &str) -> crate::source::Preset {
        crate::source::Preset {
            key: key.to_string(),
            name: self.name.clone().unwrap_or_else(|| key.to_string()),
            level: self.level.as_deref().and_then(parse_level),
            since: self.since.clone(),
        }
    }
}

fn parse_level(s: &str) -> Option<crate::log::LevelFilter> {
    match s.to_lowercase().as_str() {
        "error" => Some(crate::log::LevelFilter::Error),
        "warn" | "warning" => Some(crate::log::LevelFilter::Warn),
        "info" => Some(crate::log::LevelFilter::Info),
        "debug" => Some(crate::log::LevelFilter::Debug),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]

    use super::*;

    #[test]
    fn 预设转换_级别解析与缺省名() {
        let pc = PresetCfg {
            name: Some("仅ERROR(1h)".into()),
            level: Some("error".into()),
            since: Some("1h".into()),
        };
        let p = pc.to_preset("quick_error");
        assert_eq!(p.key, "quick_error");
        assert_eq!(p.name, "仅ERROR(1h)");
        assert_eq!(p.level, Some(crate::log::LevelFilter::Error));
        assert_eq!(p.since.as_deref(), Some("1h"));
        // 缺省名回落 key；warning 别名；非法级别按 None；无 since
        let pc2 = PresetCfg {
            name: None,
            level: Some("WARNING".into()),
            since: None,
        };
        let p2 = pc2.to_preset("k");
        assert_eq!(p2.name, "k");
        assert_eq!(p2.level, Some(crate::log::LevelFilter::Warn));
        let pc3 = PresetCfg {
            name: None,
            level: Some("灾难级".into()),
            since: None,
        };
        assert_eq!(pc3.to_preset("k").level, None);
    }
}
