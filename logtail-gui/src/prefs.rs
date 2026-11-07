//! GUI 偏好持久化：`~/.config/logtail/gui.toml`（与 TUI 的 config/state 同目录）。
//!
//! 读写走纯函数（`to_toml` / `from_toml`）便于单测；磁盘 IO 仅 load/save 两个入口。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// 近期文件上限：菜单里只显示这么多
const RECENT_MAX: usize = 10;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// 日志正文字号（也是行高基准）
    pub font_size: f32,
    /// 长行自动换行（false = 截断，与 TUI wrap_long_lines 同义）
    pub wrap: bool,
    /// 每个 tab 的行数上限（LogBuffer 容量）
    pub max_lines: usize,
    /// 窗口置顶（BareTail always-on-top）
    pub always_on_top: bool,
    /// 最近打开的文件（新→旧）
    pub recent_files: Vec<String>,
    pub window: WindowPrefs,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct WindowPrefs {
    pub w: f32,
    pub h: f32,
    pub maximized: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            font_size: 14.0,
            wrap: false,
            max_lines: 10_000,
            always_on_top: false,
            recent_files: Vec::new(),
            window: WindowPrefs { w: 1280.0, h: 800.0, maximized: false },
        }
    }
}

impl Prefs {
    pub fn path() -> PathBuf {
        logtail_core::config::config_dir().join("gui.toml")
    }

    pub fn load() -> Self {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| from_toml(&s))
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let dir = logtail_core::config::config_dir();
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        if let Some(text) = to_toml(self) {
            let _ = std::fs::write(Self::path(), text);
        }
    }

    /// 记录最近打开：去重置顶，截断到上限
    pub fn push_recent(&mut self, path: &str) {
        self.recent_files.retain(|p| p != path);
        self.recent_files.insert(0, path.to_string());
        self.recent_files.truncate(RECENT_MAX);
    }
}

pub fn to_toml(p: &Prefs) -> Option<String> {
    toml::to_string_pretty(p).ok()
}

pub fn from_toml(text: &str) -> Option<Prefs> {
    toml::from_str(text).ok()
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]

    use super::*;

    #[test]
    fn 往返_字段不丢() {
        let p = Prefs {
            font_size: 16.5,
            wrap: true,
            max_lines: 2000,
            always_on_top: true,
            recent_files: vec!["C:/logs/a.log".into(), "D:/b.out".into()],
            window: WindowPrefs { w: 800.0, h: 600.0, maximized: true },
        };
        let text = to_toml(&p).unwrap();
        let back = from_toml(&text).unwrap();
        assert_eq!(back, p);
        // 未知字段 / 缺字段容忍：只留一部分键也能解析出默认值
        let partial = "font_size = 18.0\n";
        let p2 = from_toml(partial).unwrap();
        assert_eq!(p2.font_size, 18.0);
        assert_eq!(p2.max_lines, 10_000, "缺省字段回落默认值");
    }

    #[test]
    fn 坏文件_回落默认() {
        assert!(from_toml("这不是 toml [[[").is_none());
    }

    #[test]
    fn recent_去重置顶截断() {
        let mut p = Prefs::default();
        for i in 0..(RECENT_MAX + 3) {
            p.push_recent(&format!("f{i}.log"));
        }
        assert_eq!(p.recent_files.len(), RECENT_MAX);
        assert_eq!(p.recent_files[0], format!("f{}.log", RECENT_MAX + 2), "最新在最前");
        p.push_recent("f3.log");
        assert_eq!(p.recent_files[0], "f3.log", "重复打开置顶");
        assert_eq!(p.recent_files.iter().filter(|s| **s == "f3.log").count(), 1, "去重");
    }
}
