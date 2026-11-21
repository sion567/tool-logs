//! GUI 偏好持久化：`~/.config/logtail/gui.toml`（与 TUI 的 config/state 同目录）。
//!
//! 读写走纯函数（`to_toml` / `from_toml`）便于单测；磁盘 IO 仅 load/save 两个入口。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// 近期文件上限：菜单里只显示这么多
const RECENT_MAX: usize = 10;

/// [v0.11.0] 过滤输入历史上限
const FILTER_HISTORY_MAX: usize = 10;

/// [v0.11.0] 收藏过滤（工具栏 chips）：name 是按钮标签，pattern 是过滤词
/// （文本按 `|` 拆多关键词 / regex_on 时按正则编译），与过滤框 AND 叠加。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedFilter {
    pub name: String,
    pub pattern: String,
    pub regex_on: bool,
}

/// 预置种子（首次使用 / 旧 gui.toml 无此字段时出现；删光后保存 [] 不再复活）
fn default_saved_filters() -> Vec<SavedFilter> {
    let sf = |name: &str, pattern: &str, regex_on: bool| SavedFilter {
        name: name.into(),
        pattern: pattern.into(),
        regex_on,
    };
    vec![
        sf("错误/警告", "ERROR|WARN", false),
        sf("traceId", r"traceId=\S+", true),
        sf("异常/超时", "NullPointer|Timeout|deadline", false),
    ]
}

/// [v0.2.0] 启动自动恢复的上次激活 tab（与 TUI remember_last_source 同心智）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum LastTab {
    Local { path: String },
    Remote { source: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// 日志正文字号（也是行高基准）
    pub font_size: f32,
    /// 每个 tab 的行数上限（LogBuffer 容量）
    pub max_lines: usize,
    /// [v0.11.0] 新 tab 的上下文 ±N 默认值（0=关）：命中行连前后 N 行进视图
    pub filter_ctx: usize,
    /// [v0.11.0] 命中块折叠（ctx>0 时生效）：合并后的命中块折成一行 ×N
    pub fold_hits: bool,
    /// [v0.11.0] 多行堆栈跟随：无时间戳行缩进灰显（渲染层开关，查看菜单）
    pub stack_follow: bool,
    /// [v0.11.0] 外部查看器命令模板：`{file}` 占位符替换为临时文件路径，
    /// 空 = 未配置（查看菜单项给出提示）。例：`lnav {file}`、`multitail {file}`
    pub viewer_cmd: String,
    /// [v0.11.0] 过滤输入历史（新→旧，cap 10）：过滤框回车记录，▼ 下拉回填
    pub filter_history: Vec<String>,
    /// 窗口置顶（BareTail always-on-top）
    pub always_on_top: bool,
    /// 最近打开的文件（新→旧）
    pub recent_files: Vec<String>,
    /// [v0.11.0] 收藏过滤 chips（与过滤框 AND 叠加）
    #[serde(default = "default_saved_filters")]
    pub saved_filters: Vec<SavedFilter>,
    /// [v0.2.0] 相似行折叠 ×N（默认开，同 TUI）
    pub dedupe: bool,
    /// [v0.10.8] 倒序显示（最新在顶；默认关 = 时间正序，同 TUI o 键）
    pub reverse: bool,
    /// [v0.10.10] 分屏：激活 tab 与下一个 tab 并排（默认关，同 TUI w 键）
    pub split: bool,
    /// [v0.2.0] 日志量时间轴（默认开）
    pub show_sparkline: bool,
    /// [v0.2.0] 上次激活 tab（退出时更新，启动自动恢复）
    pub last_tab: Option<LastTab>,
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
            max_lines: 10_000,
            filter_ctx: 5,
            fold_hits: false,
            stack_follow: false,
            viewer_cmd: String::new(),
            filter_history: Vec::new(),
            always_on_top: false,
            recent_files: Vec::new(),
            saved_filters: default_saved_filters(),
            dedupe: true,
            reverse: false,
            split: false,
            show_sparkline: true,
            last_tab: None,
            window: WindowPrefs {
                w: 1280.0,
                h: 800.0,
                maximized: false,
            },
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

    /// [v0.11.0] 记录过滤输入历史：去重置顶，截断到上限；空白词不记
    pub fn push_filter_history(&mut self, input: &str) {
        let s = input.trim();
        if s.is_empty() {
            return;
        }
        self.filter_history.retain(|h| h != s);
        self.filter_history.insert(0, s.to_string());
        self.filter_history.truncate(FILTER_HISTORY_MAX);
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
            max_lines: 2000,
            filter_ctx: 8,
            fold_hits: true,
            stack_follow: true,
            viewer_cmd: "lnav {file}".into(),
            filter_history: vec!["ERROR".into()],
            always_on_top: true,
            recent_files: vec!["C:/logs/a.log".into(), "D:/b.out".into()],
            saved_filters: vec![SavedFilter {
                name: "错误".into(),
                pattern: "ERROR".into(),
                regex_on: false,
            }],
            dedupe: false,
            reverse: true,
            split: true,
            show_sparkline: false,
            last_tab: Some(LastTab::Remote {
                source: "prod-api".into(),
            }),
            window: WindowPrefs {
                w: 800.0,
                h: 600.0,
                maximized: true,
            },
        };
        let text = to_toml(&p).unwrap();
        let back = from_toml(&text).unwrap();
        assert_eq!(back, p);
        // 未知字段 / 缺字段容忍：只留一部分键也能解析出默认值
        let partial = "font_size = 18.0\n";
        let p2 = from_toml(partial).unwrap();
        assert_eq!(p2.font_size, 18.0);
        assert_eq!(p2.max_lines, 10_000, "缺省字段回落默认值");
        assert_eq!(p2.filter_ctx, 5, "缺省字段回落默认值");
        // [v0.11.0] 旧 gui.toml 无 saved_filters → 预置种子；显式 [] 不复活
        assert_eq!(p2.saved_filters.len(), 3, "缺省回落种子");
        let empty = from_toml("saved_filters = []\n").unwrap();
        assert!(empty.saved_filters.is_empty(), "删光后不复活");
        // v0.1.x 旧 gui.toml（无 dedupe/last_tab）→ 默认值，不报错
        assert!(p2.dedupe, "折叠默认开");
        assert!(p2.show_sparkline, "时间轴默认开");
        assert!(!p2.reverse, "倒序默认关（时间正序）");
        assert!(!p2.split, "分屏默认关");
        assert!(p2.last_tab.is_none());
        // 本地路径 last_tab 往返
        let p3 = Prefs {
            last_tab: Some(LastTab::Local {
                path: "C:/logs/a.log".into(),
            }),
            ..Prefs::default()
        };
        let back3 = from_toml(&to_toml(&p3).unwrap()).unwrap();
        assert_eq!(
            back3.last_tab,
            Some(LastTab::Local {
                path: "C:/logs/a.log".into()
            })
        );
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
        assert_eq!(
            p.recent_files[0],
            format!("f{}.log", RECENT_MAX + 2),
            "最新在最前"
        );
        p.push_recent("f3.log");
        assert_eq!(p.recent_files[0], "f3.log", "重复打开置顶");
        assert_eq!(
            p.recent_files.iter().filter(|s| **s == "f3.log").count(),
            1,
            "去重"
        );
    }

    #[test]
    fn filter_history_去重置顶截断_空词不记() {
        let mut p = Prefs::default();
        for i in 0..(FILTER_HISTORY_MAX + 3) {
            p.push_filter_history(&format!("kw{i}"));
        }
        assert_eq!(p.filter_history.len(), FILTER_HISTORY_MAX);
        assert_eq!(p.filter_history[0], format!("kw{}", FILTER_HISTORY_MAX + 2));
        p.push_filter_history("kw3");
        assert_eq!(p.filter_history[0], "kw3", "重复输入置顶");
        assert_eq!(p.filter_history.iter().filter(|s| *s == "kw3").count(), 1);
        p.push_filter_history("   ");
        assert_eq!(p.filter_history[0], "kw3", "空白词不记录");
    }
}
