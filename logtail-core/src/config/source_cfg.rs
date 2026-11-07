//! 源配置（§8）：把 config.toml 的 [[sources] 映射到源声明

use serde::Deserialize;

use crate::source::LogSource;

#[derive(Debug, Clone, Deserialize)]
pub struct SourceConfig {
    /// 逻辑源名 = SourceId（如 "prod-api"）
    pub name: String,
    /// 所属服务器名（对应 [[servers]].name）；本地源（type="local"）可省略
    #[serde(default)]
    pub server: String,
    pub source: LogSource,
}
