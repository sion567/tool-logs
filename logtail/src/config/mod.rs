//! 配置格式（§8）

pub mod load;
pub mod server;
pub mod source_cfg;

use std::collections::BTreeMap;

use serde::Deserialize;

pub use load::{append_error_log, config_dir, load, load_state, save_state, StateFile};
pub use server::{AuthCfg, ServerConfig};
pub use source_cfg::SourceConfig;

#[derive(Debug, Default, Deserialize)]
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
