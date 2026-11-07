//! Tab 状态：缓冲 + 过滤视图 + 跟随/选中（纯逻辑，不依赖 egui，可单测）。
//!
//! 数据链路与 TUI 的 drain_session 同语义：泵整批给 `ingest` → `parse_line` → `LogBuffer.push`，
//! 视图按「总数 / 过滤 / 级别」三者任一变化才重建（对齐 TUI 的 last_rebuild 机制）。

use std::sync::Arc;

use eframe::egui;

use logtail_core::log::filter::{rebuild_view, FilterMode, LevelFilter, NO_FILTER};
use logtail_core::log::parser::parse_line;
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

/// 变高行布局缓存（v0.1.4 换行修复）：每行高度前缀和，供 show_viewport 虚拟化定位。
/// key = (视图代数, 宽度, 字号, wrap)，任一变化才重算。
#[derive(Debug, Default, Clone)]
pub struct RowLayout {
    pub width: f32,
    pub font_size: f32,
    pub wrap: bool,
    pub view_gen: u64,
    /// prefix[i] = 第 i 行顶（内容坐标），len = view.len()+1；prefix 恒非空
    pub prefix: Vec<f32>,
    pub total: f32,
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
    /// 视图重建代数：ensure_view 每次真正重建 +1（行高缓存失效依据）
    pub view_gen: u64,
    /// 变高行布局缓存（wrap 行高）
    pub layout: RowLayout,
    /// 上一帧日志区内容宽度（变高行估算用）
    pub last_inner_w: f32,

    /// 本地文件大小（状态栏）；远程 tab 显示会话状态
    pub file_size: u64,
}

impl Tab {
    pub fn new_local(id: u64, path: &str, max_lines: usize) -> Self {
        Self::new(
            id,
            TabKind::Local { path: path.to_string() },
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
            view_gen: 0,
            layout: RowLayout::default(),
            last_inner_w: 0.0,
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

    /// 泵整批送达：解析入缓冲。返回本批行数。
    pub fn ingest(&mut self, lines: Vec<String>) -> usize {
        let n = lines.len();
        for raw in lines {
            let mut line = parse_line(raw, self.src_type);
            line.source = self.title.clone();
            self.buffer.push(line);
        }
        self.total_pushed += n;
        n
    }

    /// 清屏：缓冲清空，视图强制重建
    pub fn clear(&mut self) {
        self.buffer = LogBuffer::new(self.max_lines);
        self.total_pushed += 1; // 强制 view 失效
        self.clear_selection();
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
            self.view = rebuild_view(
                &self.buffer,
                fm.as_ref().unwrap_or(&NO_FILTER),
                self.level,
            );
            self.view_built_at = self.total_pushed;
            self.view_filter = fm;
            self.view_level = self.level;
            self.view_gen += 1; // 行高缓存随之失效
        }
    }

    /// 变高行布局（v0.1.4 换行修复）：按显示列数估算每行折行数 → 前缀和。
    /// 只在 视图代数/宽度/字号/wrap 变化时重算；高估行高安全（宁可多留白不裁字）。
    pub fn ensure_layout(
        &mut self,
        width: f32,
        font_size: f32,
        wrap: bool,
        line_h: f32,
        char_w: f32,
        min_row_h: f32,
    ) {
        let n = self.view.len();
        let ok = self.layout.wrap == wrap
            && self.layout.view_gen == self.view_gen
            && (self.layout.width - width).abs() < 0.5
            && (self.layout.font_size - font_size).abs() < 0.01
            && self.layout.prefix.len() == n + 1;
        if ok {
            return;
        }
        let mut prefix = Vec::with_capacity(n + 1);
        prefix.push(0.0);
        let mut acc = 0.0f32;
        if wrap {
            // 左右各 3px 内边距 + 滚动条预留
            let cols = (((width - 6.0) / char_w).floor() as usize).max(1);
            for &no in &self.view {
                let lines = self
                    .buffer
                    .by_line_no(no)
                    .map(|l| wrap_line_count(&l.raw, cols))
                    .unwrap_or(1);
                acc += (lines as f32 * line_h + 4.0).max(min_row_h);
                prefix.push(acc);
            }
        } else {
            for _ in 0..n {
                acc += min_row_h;
                prefix.push(acc);
            }
        }
        self.layout = RowLayout {
            width,
            font_size,
            wrap,
            view_gen: self.view_gen,
            prefix,
            total: acc,
        };
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
        Ok(re) => (Some(FilterMode::Regex(Arc::new(re), input.to_string())), None),
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

/// 估算折行数（mono 网格）：该行占 visual_cols 列 → ceil(列 / 每行列数)，至少 1 行
pub fn wrap_line_count(raw: &str, cols_per_row: usize) -> usize {
    let cols = visual_cols(raw);
    cols.div_ceil(cols_per_row).max(1)
}

/// 显示宽度（列数）：CJK/全角 = 2，Tab = 4，其余 = 1。
/// 高估安全：行高只多不少（低估会把折行文本裁掉）。
pub fn visual_cols(s: &str) -> usize {
    s.chars()
        .map(|c| {
            let u = c as u32;
            if c == '\t' {
                4
            } else if matches!(
                u,
                0x1100..=0x115F
                    | 0x2E80..=0x303E
                    | 0x3041..=0x33FF
                    | 0x3400..=0x4DBF
                    | 0x4E00..=0x9FFF
                    | 0xA000..=0xA4CF
                    | 0xAC00..=0xD7A3
                    | 0xF900..=0xFAFF
                    | 0xFE10..=0xFE19
                    | 0xFE30..=0xFE6F
                    | 0xFF00..=0xFF60
                    | 0xFFE0..=0xFFE6
            ) || u >= 0x20000
            {
                2
            } else {
                1
            }
        })
        .sum()
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
                auth: logtail_core::config::AuthCfg::Password { password: "x".into() },
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
    fn 视觉列宽_折行数估算() {
        use super::{visual_cols, wrap_line_count};
        assert_eq!(visual_cols("hello"), 5);
        assert_eq!(visual_cols("中文日志"), 8, "CJK 每字 2 列");
        assert_eq!(visual_cols("a\tb"), 1 + 4 + 1, "Tab 按 4 列");
        // 每行 10 列：12 列的行 → 2 行；空串 → 1 行
        assert_eq!(wrap_line_count("abcdefghij", 10), 1);
        assert_eq!(wrap_line_count("abcdefghijkl", 10), 2);
        assert_eq!(wrap_line_count("中文中文中文中文中文中文", 10), 3, "24 列 / 10 = 3 行");
        assert_eq!(wrap_line_count("", 10), 1);
    }

    #[test]
    fn 行高布局_前缀和与失效() {
        let mut t = tab();
        t.ingest(vec![
            "short".into(),
            "a very long log line that should wrap into multiple visual rows when narrow".into(),
            "中文字符行宽度按双列估算".into(),
        ]);
        t.ensure_view();
        let g0 = t.view_gen;
        // wrap 关：每行 = min_row_h
        t.ensure_layout(800.0, 14.0, false, 20.0, 8.0, 21.0);
        assert_eq!(t.layout.prefix.len(), 4);
        assert_eq!(t.layout.total, 63.0, "3 行 × 21.0");
        assert_eq!(t.layout.view_gen, g0);
        // 参数不变 → 缓存命中（不重算）
        let old_prefix = t.layout.prefix.clone();
        t.ensure_layout(800.0, 14.0, false, 20.0, 8.0, 21.0);
        assert_eq!(t.layout.prefix, old_prefix);
        // wrap 开：长行行高 > 短行；前缀单调递增
        t.ensure_layout(300.0, 14.0, true, 20.0, 8.0, 21.0);
        assert!(t.layout.prefix.windows(2).all(|w| w[1] > w[0]), "前缀严格递增");
        assert!(t.layout.total > 63.0, "wrap 后总高更高");
        let long_row_h = t.layout.prefix[2] - t.layout.prefix[1];
        let short_row_h = t.layout.prefix[1] - t.layout.prefix[0];
        assert!(long_row_h > short_row_h, "长行折多行 → 行高更大");
        // 新数据 → view_gen 变 → 缓存失效重算
        t.ingest(vec!["another".into()]);
        t.ensure_view();
        assert_ne!(t.view_gen, g0);
        let old_total = t.layout.total;
        t.ensure_layout(300.0, 14.0, true, 20.0, 8.0, 21.0);
        assert_ne!(t.layout.total, old_total, "视图变化后重算");
    }
}
