//! Tab 状态：缓冲 + 过滤视图 + 跟随/选中（纯逻辑，不依赖 egui，可单测）。
//!
//! 数据链路与 TUI 的 drain_session 同语义：泵整批给 `ingest` → `parse_line` → `LogBuffer.push`，
//! 视图按「总数 / 过滤 / 级别」三者任一变化才重建（对齐 TUI 的 last_rebuild 机制）。

use std::sync::Arc;

use eframe::egui;

use logtail_core::log::filter::{FilterMode, LevelFilter, NO_FILTER, rebuild_view};
use logtail_core::log::parser::parse_line_multi;
use logtail_core::log::{LogBuffer, LogLevel};
use logtail_core::source::SourceType;

/// 会话状态直接复用 TUI 的定义（标签也一致）
pub use logtail_core::ssh::session::SessionState;

/// 远程 tab 携带的原始声明（重连要用）；Box 压枚举尺寸（clippy large_variant）
#[derive(Debug, Clone)]
pub struct RemoteSpec {
    pub source_name: String,
    pub server_name: String,
    pub source: logtail_core::source::LogSource,
    pub server: logtail_core::config::ServerConfig,
}

/// tab 的数据来源
#[derive(Debug, Clone)]
pub enum TabKind {
    Local { path: String },
    Remote(Box<RemoteSpec>),
}

impl TabKind {
    pub fn label(&self) -> String {
        match self {
            TabKind::Local { path } => {
                // tab 标题只取文件名，状态栏/悬停提示里看全路径
                std::path::Path::new(path)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.clone())
            }
            TabKind::Remote(r) => r.source_name.clone(),
        }
    }
}

pub struct Tab {
    pub id: u64,
    pub kind: TabKind,
    pub title: String,
    pub src_type: SourceType,
    pub buffer: LogBuffer,
    pub max_lines: usize,
    pub state: SessionState,

    // ---- 过滤输入（随输入实时生效，无需回车）----
    pub filter_input: String,
    pub regex_on: bool,
    /// 正则编译失败时的报错（状态栏红字）；文本模式恒 None
    pub regex_error: Option<String>,
    /// (input, regex_on) → 编译结果的缓存，输入不变不重编
    compiled_input: String,
    compiled_regex_on: bool,
    compiled: Option<FilterMode>,

    pub level: LevelFilter,

    // ---- 视图 ----
    pub follow: bool,
    /// 请求跳到末尾（勾选跟随/点「末尾」按钮时置位，渲染层消化后清）
    pub jump_to_end: bool,
    /// 上一帧日志区视口高度（精确跳底落点用）
    pub last_inner_h: f32,
    pub selected: Option<u64>,
    /// Shift+单击的区间终点（与 selected 组成 anchor..focus；None = 单行选中）
    pub sel_focus: Option<u64>,
    pub total_pushed: usize,
    pub view: Vec<u64>,
    view_built_at: usize,
    view_filter: Option<FilterMode>,
    view_level: LevelFilter,

    /// [v0.1.6] 行详情条：双击行 / 右键「查看完整行」打开，主视图固定单行，
    /// 长行全文在详情条内折行（对应 TUI 详情面板心智）
    pub detail_line: Option<u64>,

    /// 本地文件大小（状态栏）；远程 tab 显示会话状态
    pub file_size: u64,
}

impl Tab {
    pub fn new_local(id: u64, path: &str, max_lines: usize) -> Self {
        Self::new(
            id,
            TabKind::Local {
                path: path.to_string(),
            },
            SourceType::File,
            max_lines,
        )
    }

    /// 远程 tab：直接给已解析的源/服务器声明（重连用）
    pub fn new_remote(
        id: u64,
        sc: &logtail_core::config::SourceConfig,
        srv: &logtail_core::config::ServerConfig,
        max_lines: usize,
    ) -> Self {
        Self::new(
            id,
            TabKind::Remote(Box::new(RemoteSpec {
                source_name: sc.name.clone(),
                server_name: sc.server.clone(),
                source: sc.source.clone(),
                server: srv.clone(),
            })),
            sc.source.source_type(),
            max_lines,
        )
    }

    fn new(id: u64, kind: TabKind, src_type: SourceType, max_lines: usize) -> Self {
        let title = kind.label();
        Self {
            id,
            kind,
            title,
            src_type,
            buffer: LogBuffer::new(max_lines),
            max_lines,
            state: SessionState::Connecting,
            filter_input: String::new(),
            regex_on: false,
            regex_error: None,
            compiled_input: String::new(),
            compiled_regex_on: false,
            compiled: None,
            level: LevelFilter::All,
            follow: true,
            jump_to_end: false,
            last_inner_h: 0.0,
            selected: None,
            sel_focus: None,
            total_pushed: 0,
            view: Vec::new(),
            view_built_at: usize::MAX,
            view_filter: None,
            view_level: LevelFilter::All,
            detail_line: None,
            file_size: 0,
        }
    }

    /// 全路径（状态栏/导出名用）
    pub fn path_for_export(&self) -> String {
        match &self.kind {
            TabKind::Local { path } => path.clone(),
            TabKind::Remote(r) => r.source_name.clone(),
        }
    }

    /// 泵整批送达：解析入缓冲。返回本批入缓行数——[v0.1.6] 多行记录（内嵌 \n）
    /// 拆成独立日志行，可能多于批内原始行数。
    pub fn ingest(&mut self, lines: Vec<String>) -> usize {
        let mut n = 0usize;
        for raw in lines {
            for mut line in parse_line_multi(raw, self.src_type) {
                line.source = self.title.clone();
                self.buffer.push(line);
                n += 1;
            }
        }
        self.total_pushed += n;
        n
    }

    /// 清屏：缓冲清空，视图强制重建
    pub fn clear(&mut self) {
        self.buffer = LogBuffer::new(self.max_lines);
        self.total_pushed += 1; // 强制 view 失效
        self.clear_selection();
        self.detail_line = None; // 行已不存在，详情条一并关闭
        self.view.clear();
        self.view_built_at = self.total_pushed;
    }

    /// 当前选中区间（anchor 与 Shift 终点归一化为 (lo, hi) 含端点）；无选中 None
    pub fn selected_range(&self) -> Option<(u64, u64)> {
        let a = self.selected?;
        Some(match self.sel_focus {
            Some(f) => (a.min(f), a.max(f)),
            None => (a, a),
        })
    }

    /// 选中行数（右键菜单标签用；上限于缓冲容量——窗口外的已淘汰行不存在）
    pub fn range_count(&self) -> Option<usize> {
        let (lo, hi) = self.selected_range()?;
        Some((hi - lo) as usize)
            .map(|n| n.saturating_add(1))
            .map(|n| n.min(self.max_lines))
    }

    /// 复制文本：选中区间内仍在缓冲窗口的行按顺序 `\n` 拼接；无选中/全淘汰 → None。
    /// Ctrl+C 与右键「复制选中 N 行」共用。
    pub fn copy_text(&self) -> Option<String> {
        let (lo, hi) = self.selected_range()?;
        let mut out = String::new();
        let mut n = lo;
        let mut taken = 0usize;
        while n <= hi && taken < self.max_lines {
            if let Some(l) = self.buffer.by_line_no(n) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&l.raw);
                taken += 1;
            }
            n = n.saturating_add(1);
        }
        Some(out).filter(|s| !s.is_empty())
    }

    /// 清选择（过滤/视图变化时调用）
    pub fn clear_selection(&mut self) {
        self.selected = None;
        self.sel_focus = None;
    }

    /// 当前生效过滤（含缓存编译）；正则编译失败时返回 None 并记录 regex_error
    pub fn active_filter(&mut self) -> Option<FilterMode> {
        if self.compiled_input != self.filter_input || self.compiled_regex_on != self.regex_on {
            self.compiled_input = self.filter_input.clone();
            self.compiled_regex_on = self.regex_on;
            let (compiled, err) = compile_filter(&self.filter_input, self.regex_on);
            self.compiled = compiled;
            self.regex_error = err;
        }
        self.compiled.clone()
    }

    /// 视图按需重建（每帧调用；命中缓存只在 总数/过滤/级别 变化时重算）
    pub fn ensure_view(&mut self) {
        let fm = self.active_filter();
        if self.total_pushed != self.view_built_at
            || self.view_filter != fm
            || self.view_level != self.level
        {
            self.view = rebuild_view(&self.buffer, fm.as_ref().unwrap_or(&NO_FILTER), self.level);
            self.view_built_at = self.total_pushed;
            self.view_filter = fm;
            self.view_level = self.level;
        }
    }
}

/// (编译结果, 正则错误)。空输入 = 无过滤；文本模式原样透传（匹配时按 `|` 拆多关键词）。
fn compile_filter(input: &str, regex_on: bool) -> (Option<FilterMode>, Option<String>) {
    if input.is_empty() {
        return (None, None);
    }
    if !regex_on {
        return (Some(FilterMode::Text(input.to_string())), None);
    }
    match regex::Regex::new(input) {
        Ok(re) => (
            Some(FilterMode::Regex(Arc::new(re), input.to_string())),
            None,
        ),
        Err(e) => (None, Some(e.to_string())),
    }
}

/// 级别 → egui 颜色（BareTail 白底风格：深色字，白底上可读）
pub fn level_color(level: LogLevel) -> egui::Color32 {
    match level {
        LogLevel::Error => egui::Color32::from_rgb(192, 0, 0),
        LogLevel::Warn => egui::Color32::from_rgb(176, 106, 0),
        LogLevel::Info => egui::Color32::from_rgb(26, 122, 26),
        LogLevel::Debug => egui::Color32::from_rgb(31, 111, 168),
        LogLevel::Unknown => egui::Color32::from_rgb(51, 51, 51),
    }
}

// 供 ui 层复用：对【已拷出的原始行】判断命中（语义与 matches_filter 一致）
pub(crate) fn raw_hits(raw: &str, fm: Option<&FilterMode>) -> bool {
    match fm {
        None | Some(FilterMode::None) => true,
        Some(FilterMode::Text(t)) => logtail_core::log::filter::text_patterns(t)
            .iter()
            .any(|p| raw.contains(p)),
        Some(FilterMode::Regex(re, _)) => re.is_match(raw),
    }
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]

    use super::*;

    fn tab() -> Tab {
        Tab::new_local(1, "C:/logs/demo.log", 100)
    }

    #[test]
    fn ingest_级别识别与容量淘汰() {
        let mut t = tab();
        let n = t.ingest(vec![
            "2026-11-07 10:00:00 INFO start".into(),
            "2026-11-07 10:00:01 ERROR db refused".into(),
            "plain line".into(),
        ]);
        assert_eq!(n, 3);
        assert_eq!(t.buffer.len(), 3);
        t.ensure_view();
        assert_eq!(t.view.len(), 3);
        // 全部解析后级别正确
        let levels: Vec<_> = t.buffer.iter().map(|l| l.level).collect();
        assert_eq!(levels[0], LogLevel::Info);
        assert_eq!(levels[1], LogLevel::Error);
        assert_eq!(levels[2], LogLevel::Unknown);
    }

    #[test]
    fn 视图_文本多关键词OR与级别阈值() {
        let mut t = tab();
        t.ingest(vec![
            "ERROR timeout a".into(),
            "ERROR refused b".into(),
            "INFO ok c".into(),
        ]);
        t.filter_input = "timeout|refused".into();
        t.ensure_view();
        assert_eq!(t.view.len(), 2, "多关键词 OR 命中 2 行");
        // 改级别阈值 → 立即重建
        t.level = LevelFilter::Error;
        t.ensure_view();
        assert_eq!(t.view.len(), 2, "过滤词仍生效，两行都是 ERROR");
        t.filter_input.clear();
        t.level = LevelFilter::Warn;
        t.ensure_view();
        assert_eq!(t.view.len(), 2, "无过滤 + WARN+ = 两行 ERROR");
        t.level = LevelFilter::All;
        t.ensure_view();
        assert_eq!(t.view.len(), 3);
    }

    #[test]
    fn 视图_正则模式与编译失败() {
        let mut t = tab();
        t.ingest(vec!["refused by db".into(), "all good".into()]);
        t.regex_on = true;
        t.filter_input = "refus.*db".into();
        t.ensure_view();
        assert_eq!(t.view.len(), 1);
        assert!(t.regex_error.is_none());
        // 坏正则：无过滤生效 + 报错可见
        t.filter_input = "([".into();
        t.ensure_view();
        assert!(t.regex_error.is_some());
        assert_eq!(t.view.len(), 2, "编译失败 = 无过滤，全部可见");
    }

    #[test]
    fn 视图_随新行增量更新() {
        let mut t = tab();
        t.ingest(vec!["INFO a".into()]);
        t.ensure_view();
        assert_eq!(t.view.len(), 1);
        t.ingest(vec!["ERROR b".into(), "INFO c".into()]);
        t.ensure_view();
        assert_eq!(t.view.len(), 3, "新行进视图");
        // line_no 稳定：淘汰窗口内可直接反查
        let no = t.view[0];
        assert!(t.buffer.by_line_no(no).is_some());
    }

    #[test]
    fn 清屏_视图与选中复位() {
        let mut t = tab();
        t.ingest(vec!["INFO a".into()]);
        t.ensure_view();
        t.selected = t.view.first().copied();
        t.clear();
        assert_eq!(t.buffer.len(), 0);
        assert_eq!(t.view.len(), 0);
        assert!(t.selected.is_none());
        assert!(t.sel_focus.is_none());
        // 清屏后继续 ingest 正常
        t.ingest(vec!["ERROR b".into()]);
        t.ensure_view();
        assert_eq!(t.view.len(), 1);
    }

    #[test]
    fn 区间选择_复制拼接与淘汰缺失() {
        let mut t = tab();
        t.ingest(vec!["a".into(), "b".into(), "c".into(), "d".into()]);
        // 无选中
        assert!(t.copy_text().is_none());
        // 单行
        t.selected = Some(1);
        assert_eq!(t.copy_text().as_deref(), Some("b"));
        assert_eq!(t.range_count(), Some(1));
        // 区间：anchor=1 Shift 到 3（反方向也归一化）
        t.sel_focus = Some(3);
        assert_eq!(t.selected_range(), Some((1, 3)));
        assert_eq!(t.range_count(), Some(3));
        assert_eq!(t.copy_text().as_deref(), Some("b\nc\nd"));
        // 反向：anchor=3 focus=1
        t.selected = Some(3);
        t.sel_focus = Some(1);
        assert_eq!(t.selected_range(), Some((1, 3)));
        assert_eq!(t.copy_text().as_deref(), Some("b\nc\nd"));
        // 淘汰缺失：容量 2 只剩 no=2,3（c,d）——区间 0..3 只拼出存在的行
        let mut small = Tab::new_local(2, "C:/logs/s.log", 2);
        small.ingest(vec!["a".into(), "b".into(), "c".into(), "d".into()]);
        small.selected = Some(0);
        small.sel_focus = Some(3);
        assert_eq!(small.copy_text().as_deref(), Some("c\nd"), "淘汰行跳过");
        assert_eq!(small.range_count(), Some(2), "计数上限于容量");
        // 清选择
        t.clear_selection();
        assert!(t.copy_text().is_none());
    }

    #[test]
    fn tab标题_本地取文件名_远程取源名() {
        let t = tab();
        assert_eq!(t.title, "demo.log");
        assert_eq!(t.path_for_export(), "C:/logs/demo.log");
        let r = Tab::new_remote(
            2,
            &logtail_core::config::SourceConfig {
                name: "prod-api".into(),
                server: "prod-01".into(),
                source: logtail_core::source::LogSource::Docker {
                    container: "my-app".into(),
                    tail: 100,
                    follow: true,
                    since: None,
                    until: None,
                },
            },
            &logtail_core::config::ServerConfig {
                name: "prod-01".into(),
                host: "1.2.3.4".into(),
                port: 22,
                user: "root".into(),
                auth: logtail_core::config::AuthCfg::Password {
                    password: "x".into(),
                },
                sudo: false,
                sudo_password: None,
            },
            10,
        );
        assert_eq!(r.title, "prod-api");
        assert!(matches!(r.kind, TabKind::Remote(_)));
        assert_eq!(r.src_type, SourceType::Docker, "docker 源拆 JSON 包壳");
    }

    #[test]
    fn ingest_多行记录拆分_子行继承时间戳() {
        // [v0.1.6] docker JSON 记录内嵌 \n（一条记录打印多行内容）→ 拆成独立日志行；
        // 子行继承记录时间戳、独立识别级别；单行批不受影响
        let mut t = Tab::new_remote(
            9,
            &logtail_core::config::SourceConfig {
                name: "prod-api".into(),
                server: "prod-01".into(),
                source: logtail_core::source::LogSource::Docker {
                    container: "my-app".into(),
                    tail: 100,
                    follow: true,
                    since: None,
                    until: None,
                },
            },
            &logtail_core::config::ServerConfig {
                name: "prod-01".into(),
                host: "1.2.3.4".into(),
                port: 22,
                user: "root".into(),
                auth: logtail_core::config::AuthCfg::Password {
                    password: "x".into(),
                },
                sudo: false,
                sudo_password: None,
            },
            10,
        );
        let n = t.ingest(vec![
            concat!(
                r#"{"log":"{\"id\":-1}\n"#,
                r#"ERROR 崩了\n","#,
                r#""stream":"stdout","time":"2026-01-05T14:02:04.123+08:00"}"#
            )
            .into(),
        ]);
        assert_eq!(n, 2, "一条记录拆 2 行");
        assert_eq!(t.buffer.len(), 2);
        assert_eq!(t.buffer.by_line_no(0).unwrap().raw, r#"{"id":-1}"#);
        let ts = t.buffer.by_line_no(0).unwrap().timestamp.unwrap();
        assert_eq!(
            t.buffer.by_line_no(1).unwrap().timestamp,
            Some(ts),
            "子行继承时间戳"
        );
        assert_eq!(t.buffer.by_line_no(1).unwrap().level, LogLevel::Error);
        assert_eq!(t.buffer.by_line_no(1).unwrap().source, t.title);
        assert_eq!(t.ingest(vec!["plain".into()]), 1, "单行批不受影响");
        assert_eq!(t.buffer.len(), 3);
        // 清屏连详情条一并关
        t.detail_line = Some(0);
        t.clear();
        assert!(t.detail_line.is_none());
    }
}
