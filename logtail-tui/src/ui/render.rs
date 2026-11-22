//! 渲染（§12.3 伪代码的落地）：标题 / 侧栏 / 工具栏 / 日志区 / 状态栏
//! （详情面板 Day6 接入）

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use ratatui::Frame;

use crate::app::{AppState, SessionState};
use crate::ui::layout::layout;
use logtail_core::log::{rebuild_view, FilterMode};

/// [core 化] 级别 → ratatui 色（原 LogLevel::color()，随渲染层走；core 不依赖 ratatui）
fn level_color(level: logtail_core::log::LogLevel) -> Color {
    match level {
        logtail_core::log::LogLevel::Error => Color::Red,
        logtail_core::log::LogLevel::Warn => Color::Yellow,
        logtail_core::log::LogLevel::Info => Color::Green,
        logtail_core::log::LogLevel::Debug => Color::Cyan,
        logtail_core::log::LogLevel::Unknown => Color::White,
    }
}

pub fn render(frame: &mut Frame, st: &mut AppState) {
    let area = frame.size();
    // [v0.8.5] 最小尺寸守卫：窄窗下固定布局会溢出绘制（碎片/面板顶出屏幕）——
    // 全屏单条提示，杜绝一切溢出渲染
    if area.width < crate::ui::layout::MIN_WIDTH || area.height < crate::ui::layout::MIN_HEIGHT {
        let msg = format!(
            "终端太小（{}x{}）：至少 {}x{} 才能显示，请拉大窗口",
            area.width,
            area.height,
            crate::ui::layout::MIN_WIDTH,
            crate::ui::layout::MIN_HEIGHT
        );
        frame.render_widget(
            ratatui::widgets::Paragraph::new(ratatui::text::Line::from(
                ratatui::text::Span::styled(
                    msg,
                    ratatui::style::Style::default()
                        .fg(ratatui::style::Color::Red)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ),
            ))
            .alignment(ratatui::layout::Alignment::Center),
            area,
        );
        return;
    }
    // [v0.4 P1#6] has_detail 从状态取
    let out = layout(area, st.detail_panel.is_some(), &st.ui);

    // [v0.4 P2#2] 命中缓存：仅 last_rebuild 变化（入行/过滤变更）才全量重扫，其余帧直接复用
    if st.view_cache.built_at != st.last_rebuild {
        let mut hits = rebuild_view(st.current_buffer(), st.active_filter(), st.level_filter);
        // C 清屏（Day7）：只清显示标记，line_no <= cleared_before 的行不再显示
        if let Some(cut) = st.cleared_before {
            hits.retain(|&no| no > cut);
        }
        // [v0.7] sparkline 随命中缓存一并刷新；宽度随日志区（缩放后长度不符也重算）
        let width = out.log.width.max(1) as usize;
        let spark = if st.view_cache.spark.len() != width {
            crate::app::sparkline(st.current_buffer(), width, 1)
        } else {
            st.view_cache.spark.clone()
        };
        st.view_cache = crate::app::ViewCache {
            built_at: st.last_rebuild,
            hits,
            spark,
        };
    }
    // [v0.10.10] 分屏副栏命中缓存：过滤/级别/折叠两栏共享，逐栏独立的是源/清屏标记
    // （副栏不需要 spark——时间轴只反映焦点栏）
    let fm = st.active_filter().clone();
    let level = st.level_filter;
    if let Some(sp) = st.split.as_mut() {
        if sp.bg.view_cache.built_at != st.last_rebuild {
            let mut hits = sp
                .bg
                .source
                .as_ref()
                .and_then(|sid| st.buffers.get(sid))
                .map(|buf| rebuild_view(buf, &fm, level))
                .unwrap_or_default();
            if let Some(cut) = sp.bg.cleared_before {
                hits.retain(|&no| no > cut);
            }
            sp.bg.view_cache = crate::app::ViewCache {
                built_at: st.last_rebuild,
                hits,
                spark: Vec::new(),
            };
        }
    }

    render_title(frame, out.title, st);
    // [v0.9] 侧栏/sparkline 可隐藏（b/v 热键）
    if st.ui.show_sidebar {
        render_sidebar(frame, out.left, st);
    }
    render_toolbar(frame, out.toolbar, st);
    if st.ui.show_sparkline {
        render_sparkline(frame, out.spark, st); // [v0.7.6→v0.8.4 恢复] 日志量时间轴
    }
    // [v0.10.10] 分屏：单栏 = 焦点栏占满日志区；双栏 = split_log 对半 + 每栏栏头
    match st.split.as_ref() {
        None => {
            // 日志区高度记录给输入层（↑↓ 保证选中行可见需要知道窗口大小）
            st.viewport.height = out.log.height;
            render_logs(frame, out.log, st, focused_pane(st));
        }
        Some(sp) => {
            let (a, b) = crate::ui::layout::split_log(out.log);
            let (focused_rect, bg_rect) = if sp.bg_left { (b, a) } else { (a, b) };
            let focused_rows = pane_rows(focused_rect);
            // 焦点栏行数记给输入层（↑↓/PageUp 可见性计算基准）
            st.viewport.height = focused_rows.height;
            draw_pane_header(frame, focused_rect, st.active_source.as_deref(), st.following, true);
            draw_pane_header(frame, bg_rect, sp.bg.source.as_deref(), sp.bg.following, false);
            let bg_rows = pane_rows(bg_rect);
            if sp.bg.source.is_none() {
                // 未连接占位：引导文案（连接永远进焦点栏——先 n 换焦点再连接）
                frame.render_widget(
                    Paragraph::new(vec![
                        Line::from("副栏未连接"),
                        Line::from(Span::styled(
                            "n 换焦点到此栏 → Tab 进侧栏选源回车",
                            Style::default().fg(Color::DarkGray),
                        )),
                    ]),
                    bg_rows,
                );
            } else {
                render_logs(frame, bg_rows, st, bg_pane(sp));
            }
            render_logs(frame, focused_rows, st, focused_pane(st));
        }
    }

    // 详情面板（Day6→v0.10.6）：时间/级别/源/原始行/trace_id——只显示当前行（±N 上下文撤销）
    if let (Some(rect), Some(panel)) = (out.detail, st.detail_panel.clone()) {
        render_detail(frame, rect, st, &panel);
    }
    render_statusbar(frame, out.statusbar, st);
}

/// [v0.10.10] 栏头占 1 行，其余给日志行
fn pane_rows(rect: ratatui::layout::Rect) -> ratatui::layout::Rect {
    ratatui::layout::Rect {
        x: rect.x,
        y: rect.y + 1,
        width: rect.width,
        height: rect.height.saturating_sub(1),
    }
}

/// [v0.10.10] 分屏栏头：`[源名] ● 跟随/‖ 暂停`；焦点栏蓝底反白，副栏暗灰
/// （符号沿用工具栏既有 ●/‖——均已真机验证无错位）
fn draw_pane_header(
    frame: &mut Frame,
    rect: ratatui::layout::Rect,
    source: Option<&str>,
    following: bool,
    focused: bool,
) {
    let name = source.unwrap_or("未连接");
    let state = if following { "● 跟随" } else { "‖ 暂停" };
    let mut style = if focused {
        Style::default().bg(Color::Blue).fg(Color::White)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    if focused {
        style = style.add_modifier(Modifier::BOLD);
    }
    let line = Line::from(Span::styled(format!("[{name}] {state}"), style));
    frame.render_widget(Paragraph::new(line), rect);
}

/// [v0.10.10] 焦点栏渲染视图：复用既有全局字段（键盘层/连接层天然作用于焦点栏）
fn focused_pane(st: &AppState) -> PaneView<'_> {
    PaneView {
        source: st.active_source.as_deref(),
        cache: &st.view_cache,
        following: st.following,
        scroll_y: st.viewport.scroll_y,
        scroll_x: st.viewport.scroll_x,
        selected: st.selected_line_no,
    }
}

/// [v0.10.10] 副栏渲染视图：来自 SplitView.bg
fn bg_pane(sp: &crate::app::SplitView) -> PaneView<'_> {
    PaneView {
        source: sp.bg.source.as_deref(),
        cache: &sp.bg.view_cache,
        following: sp.bg.following,
        scroll_y: sp.bg.scroll_y,
        scroll_x: sp.bg.scroll_x,
        selected: sp.bg.selected_line_no,
    }
}

fn render_title(frame: &mut Frame, area: ratatui::layout::Rect, st: &AppState) {
    // [v0.5] 标题栏放 tab 条：[源名] 按连接顺序排列，激活 tab 反白高亮，
    // 每个带会话状态符（●流中 ⟳连接中 ■结束 ✗失败）
    let mut spans = vec![Span::styled(
        format!("远程日志查看器 v{}", env!("CARGO_PKG_VERSION")),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )];
    if st.tabs.is_empty() {
        spans.push(Span::raw("   未连接（Tab 聚焦侧栏选源回车）"));
    }
    for tab in &st.tabs {
        // [v0.8.7 落地补全] 状态词文字化（当时只改了文档，tab 条代码一直还是 v0.8.2 的
        // 单字符 c/x/!）：流中保持 *[源名]；非流中 [源名·已结束] 等（SessionState::tab_label）
        let label = match st.session_states.get(tab).copied() {
            Some(SessionState::Streaming) => format!("*[{tab}] "),
            Some(s) => format!("[{tab}{}] ", s.tab_label()),
            None => format!("[{tab}] "),
        };
        let active = st.active_source.as_deref() == Some(tab.as_str());
        spans.push(Span::styled(
            label,
            if active {
                Style::default()
                    .bg(Color::Blue)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            },
        ));
    }
    let line = Line::from(spans);
    frame.render_widget(
        Paragraph::new(line).block(Block::default().borders(Borders::ALL)),
        area,
    );
}

fn render_sidebar(frame: &mut Frame, area: ratatui::layout::Rect, st: &AppState) {
    // 服务器树 + 源列表 + 预设（§12.1 左栏）
    let entries = st.sidebar_entries();
    let mut lines = Vec::with_capacity(entries.len());
    for (i, (entry, label)) in entries.iter().enumerate() {
        let line = match entry {
            crate::app::SideEntry::Server(_) => Line::from(Span::styled(
                label.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            )),
            _ => {
                let selected = st.sidebar.focus && i == st.sidebar.sel;
                let style = if selected {
                    Style::default().bg(Color::Blue).fg(Color::White)
                } else if !st.sidebar.focus && i == st.sidebar.sel {
                    Style::default().fg(Color::Gray)
                } else {
                    Style::default()
                };
                Line::from(Span::styled(label.clone(), style))
            }
        };
        lines.push(line);
    }
    if !st.sidebar.focus {
        lines.push(Line::from(Span::styled(
            "(Tab 聚焦)",
            Style::default().fg(Color::DarkGray),
        )));
    }
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("源")),
        area,
    );
}

fn render_toolbar(frame: &mut Frame, area: ratatui::layout::Rect, st: &AppState) {
    let follow = if st.following {
        "● 跟随"
    } else {
        "‖ 暂停"
    };
    let regex = if st.regex_mode {
        "正则:开"
    } else {
        "正则:关"
    };
    // 过滤框显示（§12.1）：Typing 显示输入+光标；Error 红色提示；其余显示生效过滤
    let (filter_label, filter_style) = match &st.filter_state {
        logtail_core::log::FilterState::Typing { input, cursor } => (
            render_input(input, *cursor),
            Style::default().fg(Color::Yellow),
        ),
        logtail_core::log::FilterState::Error(_) => (
            "!正则错误,Esc/Enter 继续编辑".into(),
            Style::default().fg(Color::Red),
        ),
        _ => {
            let t = match st.active_filter() {
                FilterMode::None => "无".to_string(),
                FilterMode::Text(t) => t.clone(),
                FilterMode::Regex(_, t) => format!("re:{t}"),
                // [v0.11.0] AND 链仅 GUI 收藏过滤产生，TUI 兜底显示
                FilterMode::All(_) => "多条件".to_string(),
            };
            (t, Style::default())
        }
    };
    let mut spans = vec![
        Span::raw("模式:"),
        Span::styled(st.mode.label(), Style::default().fg(Color::Magenta)),
        Span::raw("  级别:"),
        Span::styled(st.level_filter.label(), Style::default().fg(Color::Cyan)),
        Span::raw("  过滤:["),
        Span::styled(filter_label, filter_style),
        Span::raw("]  "),
        Span::raw(regex),
        Span::raw(format!("  跟随:{follow}")),
    ];
    // [v0.6 §16.6] 远端预过滤指示（显式警示，紫红加粗）
    if let Some(p) = &st.remote_prefilter {
        spans.push(Span::styled(
            format!("  远筛:{p}"),
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ));
    }
    let line = Line::from(spans);
    frame.render_widget(Paragraph::new(line), area);
}

/// Typing 输入框：▌ 在光标字符位（字符计数）
fn render_input(input: &str, cursor: usize) -> String {
    let chars: Vec<char> = input.chars().collect();
    let c = cursor.min(chars.len());
    let mut s = String::new();
    for (i, ch) in chars.iter().enumerate() {
        if i == c {
            s.push('|');
        }
        s.push(*ch);
    }
    if c >= chars.len() {
        s.push('|');
    }
    s
}

/// [v0.7/v0.8.4 恢复] 日志量时间轴：按秒分桶，密度字符 ` .:-=+*#@`（末端=最新）。
/// 全 ASCII——块字符 ▁▂█ 是 East-Asian-ambiguous 宽度，Windows 终端错位源（v0.8.2）。
fn render_sparkline(frame: &mut Frame, area: ratatui::layout::Rect, st: &AppState) {
    const BARS: [char; 9] = [' ', '.', ':', '-', '=', '+', '*', '#', '@'];
    let v = &st.view_cache.spark;
    if v.is_empty() || v.iter().all(|&c| c == 0) {
        return;
    }
    let max = v.iter().max().copied().unwrap_or(1).max(1);
    let bar: String = v
        .iter()
        .map(|&c| {
            if c == 0 {
                BARS[0]
            } else {
                let lvl = ((c as f64 / max as f64) * 7.0).ceil() as usize;
                BARS[lvl.clamp(1, 8)]
            }
        })
        .collect();
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            bar,
            Style::default().fg(Color::Cyan),
        ))),
        area,
    );
}

/// [v0.10.10] 分屏渲染视图（借用，不拥有数据）：焦点栏由既有全局字段构造
/// （focused_pane），副栏由 SplitView.bg 构造（bg_pane）。窗口切片公式/折叠/
/// 倒序/滚动条与单栏完全一致——过滤/级别/折叠共享，逐栏独立的是
/// 源/跟随/滚动/选中/清屏标记。
struct PaneView<'a> {
    source: Option<&'a str>,
    cache: &'a crate::app::ViewCache,
    following: bool,
    scroll_y: u16,
    scroll_x: u16,
    selected: Option<u64>,
}

fn render_logs(frame: &mut Frame, area: ratatui::layout::Rect, st: &AppState, pane: PaneView) {
    // 行区高度即窗口行数（单栏 = out.log；分屏 = 各栏减去栏头的行区）
    let visible = area.height as usize;
    // [v0.7.6 修复] 窗口 = hits[end-visible .. end]，end = len - scroll_y。
    // 旧实现内容取到末尾 + scroll(scroll_y) 跳过前 scroll_y 行——两者恰好抵消，
    // 屏幕永远显示末尾 visible 行（用户反馈"光标上去了内容不变化"的根因）。
    // 现在精确切片，纵向 scroll 恒为 0；scroll_y 只由窗口计算表达。
    let scroll_y = if pane.following { 0 } else { pane.scroll_y } as usize;
    let hits = &pane.cache.hits;
    let len = hits.len();
    let end = len.saturating_sub(scroll_y);
    let start = end.saturating_sub(visible);
    let slice = &hits[start..end.max(start)];
    let buf = pane.source.and_then(|sid| st.buffers.get(sid));

    let fm = st.active_filter().clone();
    let mut lines = Vec::with_capacity(slice.len());
    // [v0.10] 相似行折叠：连续相同消息体（剥时间戳后）合并为一行，行尾 ` ×N`。
    // [v0.10.8] 倒序（o 键）：同一窗口切片反向渲染（最新在顶），折叠自切片尾向前
    // 吞并——等值游程对称，分组集合与正序一致，只是代表行取组内最新行
    // （倒序下显示组内最新时间戳更自然）；选中判断是整组 any，两序一致。
    if st.ui.reverse {
        let mut idx = slice.len();
        while idx > 0 {
            let Some(line) = buf.and_then(|b| b.by_line_no(slice[idx - 1])) else {
                idx -= 1;
                continue;
            };
            // 向前吞并 body 相同的相邻行
            let mut group = 1usize;
            if st.ui.dedupe {
                while idx > group {
                    let Some(prev) = buf.and_then(|b| b.by_line_no(slice[idx - 1 - group])) else {
                        break;
                    };
                    if !line.body().is_empty() && prev.body() == line.body() && prev.level == line.level {
                        group += 1;
                    } else {
                        break;
                    }
                }
            }
            let count = group;
            let selected = slice[idx - count..idx]
                .iter()
                .any(|&n| Some(n) == pane.selected);
            idx -= count;
            lines.push(log_row(line, count, selected, &fm, pane.scroll_x));
        }
    } else {
        let mut idx = 0usize;
        while idx < slice.len() {
            let Some(line) = buf.and_then(|b| b.by_line_no(slice[idx])) else {
                idx += 1;
                continue;
            };
            // 向后吞并 body 相同的相邻行
            let mut group = 1usize;
            if st.ui.dedupe {
                while idx + group < slice.len() {
                    let Some(next) = buf.and_then(|b| b.by_line_no(slice[idx + group])) else {
                        break;
                    };
                    if !line.body().is_empty() && next.body() == line.body() && next.level == line.level {
                        group += 1;
                    } else {
                        break;
                    }
                }
            }
            let count = group;
            let selected = slice[idx..idx + count].iter().any(|&n| Some(n) == pane.selected);
            idx += count;
            lines.push(log_row(line, count, selected, &fm, pane.scroll_x));
        }
    }
    // [v0.7.6] 横向滚动：渲染前对 raw 做字符级 skip——级别前缀固定不动，
    // 长行尾部随 ←/→ 可见。[v0.10.5] 主视图固定单行（截断+横滚），长行全文看详情面板
    let para = Paragraph::new(lines);
    frame.render_widget(para, area);
    // 位置滚动条（§12.1 / Day3 任务清单）：thumb 表示当前视口在 hits 中的位置。
    // [v0.10.8] 倒序：thumb 从顶部计（顶 = 最新），position = 已滚出的行数
    let sb_pos = if st.ui.reverse {
        scroll_y.min(len)
    } else {
        start
    };
    let mut sb_state = ScrollbarState::new(len).position(sb_pos);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(Some("|"))
            .thumb_symbol("#"),
        area,
        &mut sb_state,
    );
}

/// 单行渲染（[v0.10.8] 正序/倒序两分支共用）：级别前缀 + 关键词反白 + ×N 后缀
/// + 横滚字符级 skip
fn log_row(
    line: &logtail_core::log::LogLine,
    count: usize,
    selected: bool,
    fm: &FilterMode,
    scroll_x: u16,
) -> Line<'static> {
    // [v0.11.0] 颜色/语义分离：推断级（非显式 token，类名误染来源）降 DarkGray；
    // 过滤语义仍以 line.level 为准不受影响
    let fg = if line.level_explicit {
        level_color(line.level)
    } else {
        Color::DarkGray
    };
    let base = Style::default().fg(fg);
    let mut spans = vec![Span::styled(
        format!("{} ", line.level.label()),
        if selected {
            base.bg(Color::DarkGray).add_modifier(Modifier::BOLD)
        } else {
            base
        },
    )];
    // 关键词反白（过滤文本高亮，§12.3）；[v0.7.6] 横滚：raw 跳过前 scroll_x 字符
    // [v0.10] 折叠组 >1 时行尾追加 ×N
    let suffix = if count > 1 {
        format!("  ×{count}")
    } else {
        String::new()
    };
    let raw_view: String = if scroll_x > 0 {
        format!(
            "{}{suffix}",
            line.raw
                .chars()
                .skip(scroll_x as usize)
                .collect::<String>()
        )
    } else {
        format!("{}{suffix}", line.raw)
    };
    for (seg, matched) in split_matches(&raw_view, fm) {
        let style = match (matched, selected) {
            (true, true) => base
                .bg(Color::DarkGray)
                .add_modifier(Modifier::REVERSED | Modifier::BOLD),
            (true, false) => base.add_modifier(Modifier::REVERSED),
            (false, true) => base.bg(Color::DarkGray).add_modifier(Modifier::BOLD),
            (false, false) => base,
        };
        spans.push(Span::styled(seg.to_string(), style));
    }
    Line::from(spans)
}

/// 按 FilterMode 把原文切成 (片段, 是否命中) 序列，用于反白高亮
fn split_matches<'a>(text: &'a str, fm: &FilterMode) -> Vec<(&'a str, bool)> {
    match fm {
        FilterMode::None => vec![(text, false)],
        // [v0.11.0] AND 链仅 GUI 产生，TUI 兜底不做片段高亮
        FilterMode::All(_) => vec![(text, false)],
        FilterMode::Text(t) => {
            // [v0.10] 多关键词：全部命中区间合并后高亮（重叠取并集）
            let patterns = logtail_core::log::text_patterns(t);
            if patterns.is_empty() || patterns.iter().all(|p| p.is_empty()) {
                return vec![(text, false)];
            }
            let len = text.len();
            let mut hit = vec![false; len];
            for p in &patterns {
                let mut start = 0;
                while let Some(pos) = text[start..].find(p) {
                    let b = start + pos;
                    for h in &mut hit[b..(b + p.len()).min(len)] {
                        *h = true;
                    }
                    start = b + p.len().max(1);
                }
            }
            let mut out = Vec::new();
            let mut i = 0;
            while i < len {
                let h = hit[i];
                let mut j = i;
                while j < len && hit[j] == h {
                    j += 1;
                }
                // UTF-8 字符边界对齐（相对/绝对偏移 bug 修复：直接推进到边界）
                let mut seg_end = j.min(len);
                while seg_end < len && !text.is_char_boundary(seg_end) {
                    seg_end += 1;
                }
                out.push((&text[i..seg_end], h));
                i = seg_end;
            }
            out
        }
        FilterMode::Regex(re, _) => {
            let mut out = Vec::new();
            let mut last = 0;
            for m in re.find_iter(text) {
                let (s, e) = (m.start(), m.end());
                if s > last {
                    out.push((&text[last..s], false));
                }
                out.push((&text[s..e], true));
                last = e;
            }
            if last < text.len() {
                out.push((&text[last..], false));
            }
            out
        }
    }
}

fn render_statusbar(frame: &mut Frame, area: ratatui::layout::Rect, st: &AppState) {
    let hint = if st.detail_panel.is_some() {
        // [v0.7.5] 面板开着：其余键照常（↑↓ 换行后面板跟随刷新）
        "详情: q:关 c:复制行 C:复制上下文 t:trace p:源 | ↑↓:换行 R:重连 /:过滤 Esc:关".to_string()
    } else if st.sidebar.focus {
        "侧栏: ↑↓:选 Enter:连接 Tab/Esc:返回 q:退出侧栏".to_string()
    } else {
        let base = "Tab:侧栏 q:退出 /:过滤 Ctrl+R:正则 f:模式 R:重连 l:更早 e:导出 p:远筛 c:清屏 S:折叠 o:倒序 w:分屏 ←→:横滚 g:最新";
        // [v0.10.10] 分屏开着才提示换栏键
        if st.split.is_some() {
            format!("{base} n:换栏")
        } else {
            base.to_string()
        }
    };
    let (n, hits) = (st.current_buffer().len(), st.view_cache.hits.len());
    // 断网/认证失败/超时 → 红字（§四 Day7），正常消息灰底黑字
    let failed = st.status_msg.contains("失败")
        || st.status_msg.contains("超时")
        || st.status_msg.contains("权限拒绝");
    let msg_span = if failed {
        Span::styled(
            st.status_msg.clone(),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::raw(st.status_msg.clone())
    };
    // 计数放最前：窄终端截断时保住关键信息（提示语牺牲尾部）
    // [v0.7.1] 连接正常但零日志：主动给出原因提示（最常见的"没数据"是 --since 滤掉了历史）
    let mut extra = String::new();
    if n == 0
        && st
            .session_states
            .get(st.active_source.as_deref().unwrap_or(""))
            == Some(&crate::app::SessionState::Streaming)
    {
        extra = match &st.mode {
            logtail_core::source::ModeVariant::Quick { since: Some(s) } => {
                format!("  |  ! 当前只回放最近 {s}，期间无日志。按 f 切全量回放全部历史，或 L 前移")
            }
            _ => "  |  ! 已连接但暂无日志，等待新输出…（按 L 回放更早）".into(),
        };
    }
    let mut spans = vec![
        Span::raw(format!("{n} 行 {hits} 匹配")),
        Span::raw("  |  "),
        msg_span,
    ];
    // [v0.9] 键位提示可隐藏（h 键）
    if st.ui.show_hints {
        spans.push(Span::raw(format!("{extra}  |  ")));
        spans.push(Span::styled(hint, Style::default().fg(Color::DarkGray)));
    }
    let line = Line::from(spans);
    frame.render_widget(Paragraph::new(line), area);
}

/// 详情面板（§12.1 底部）：时间 / 级别 / 源 / 原始行 / trace_id / 操作提示。
/// [v0.10.6] 真机反馈：±N 上下文展示"有点奇怪"→ 只显示当前行；
/// 带走一段现场用 `C`（复制上下文块，仍按 context_before/after 取）。
/// 跟随：↑↓ 改选中后 move_selection 同步 panel.line_no，面板内容随之刷新。
fn render_detail(
    frame: &mut Frame,
    area: ratatui::layout::Rect,
    st: &AppState,
    panel: &crate::app::DetailPanel,
) {
    let buf = st.current_buffer();
    let mut lines: Vec<Line> = Vec::new();

    if let Some(line) = buf.by_line_no(panel.line_no) {
        let ts = line
            .timestamp
            .map(|t| t.format("%Y-%m-%d %H:%M:%S%.3f").to_string())
            .unwrap_or_else(|| "-".into());
        lines.push(Line::from(vec![
            Span::styled("时间: ", Style::default().fg(Color::DarkGray)),
            Span::raw(ts.clone()),
            Span::raw("   级别: "),
            Span::styled(
                line.level.label().trim(),
                Style::default().fg(level_color(line.level)),
            ),
            Span::raw(format!(
                "   源: {}",
                if line.source.is_empty() {
                    "-"
                } else {
                    &line.source
                }
            )),
        ]));
        if let Some(tid) = &line.trace_id {
            lines.push(Line::from(vec![
                Span::styled("trace_id: ", Style::default().fg(Color::DarkGray)),
                Span::styled(tid.clone(), Style::default().fg(Color::Cyan)),
            ]));
        }
        lines.push(Line::from(vec![
            Span::styled("原始: ", Style::default().fg(Color::DarkGray)),
            Span::raw(line.raw.clone()),
        ]));
    } else {
        lines.push(Line::from(Span::styled(
            "该行已被淘汰（buffer 窗口外）",
            Style::default().fg(Color::Red),
        )));
    }
    lines.push(Line::from(Span::styled(
        "q:关闭 c:复制行 C:复制上下文块 t:trace过滤 p:聚焦源",
        Style::default().fg(Color::DarkGray),
    )));

    frame.render_widget(
        // [v0.10.5] 开折行：长行全文在面板宽度内折行显示（高度按可用空间切分，
        // 与内容行数无关，不会顶爆布局）
        Paragraph::new(lines)
            .wrap(ratatui::widgets::Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(format!(
                "选中行详情 line_no={}  q:关 c:复制行 C:复制上下文±{} t:trace p:源",
                panel.line_no, panel.context_after
            ))),
        area,
    );
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]

    use super::*;
    use ratatui::backend::TestBackend;

    /// 构造带 3 行日志的测试状态（remember_last_source 关掉，避免读真实 state.toml）
    fn test_state() -> AppState {
        let mut cfg = logtail_core::config::Config::default();
        cfg.general.remember_last_source = false;
        let mut st = AppState::new(cfg);
        let mut buf = logtail_core::log::LogBuffer::new(100);
        let now = chrono::Utc::now();
        let rows = [
            (logtail_core::log::LogLevel::Info, "start sync", 2i64),
            (
                logtail_core::log::LogLevel::Error,
                "db error: connection refused",
                0,
            ),
            (logtail_core::log::LogLevel::Warn, "slow query 1.2s", 0),
        ];
        for (lvl, raw, ago) in rows {
            let mut line = logtail_core::log::LogLine {
                level: lvl,
                raw: raw.into(),
                ..Default::default()
            };
            line.source = "test-src".into();
            line.timestamp = Some(now - chrono::Duration::seconds(ago));
            buf.push(line);
        }
        st.buffers.insert("test-src".into(), buf);
        st.sources.insert(
            "test-src".into(),
            logtail_core::config::SourceConfig {
                name: "test-src".into(),
                server: "local".into(),
                source: logtail_core::source::LogSource::File {
                    path: "/var/log/x.log".into(),
                    tail: 10,
                },
            },
        );
        st.active_source = Some("test-src".into());
        st.tabs.push("test-src".into()); // [v0.5] tab 条需要已连接列表
        st.session_states
            .insert("test-src".into(), crate::app::SessionState::Streaming);
        st.last_rebuild += 1; // 与真实路径一致：buffer 入行必 bump，否则缓存视为未失效
        st
    }

    fn draw(st: &mut AppState) -> ratatui::Terminal<TestBackend> {
        let backend = TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, st)).unwrap();
        terminal
    }

    fn rows(t: &ratatui::Terminal<TestBackend>) -> Vec<String> {
        let buf = t.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf.get(x, y).symbol())
                    .collect::<String>()
            })
            .collect()
    }

    /// ratatui 宽字符占 2 格且以空白填充，CJK 断言先去掉所有空格
    fn flat(s: &str) -> String {
        s.replace(' ', "")
    }

    #[test]
    fn 渲染_标题侧栏日志状态栏齐全() {
        let mut st = test_state();
        let t = draw(&mut st);
        let rs = rows(&t);
        let all = rs.join("\n");
        let f = flat(&all);
        assert!(
            f.contains(&format!("远程日志查看器v{}", env!("CARGO_PKG_VERSION"))),
            "标题: {all}"
        );
        assert!(all.contains("test-src"), "侧栏源名: {all}");
        assert!(
            all.contains("db error: connection refused"),
            "日志行: {all}"
        );
        assert!(f.contains("3行3匹配"), "状态栏计数: {all}");
        // 滚动条（Day3 任务项）：日志区最右列应有轨道/拇指符号
        let buf = t.backend().buffer();
        let right = buf.area.width - 1;
        let sb_glyphs: Vec<String> = (4..buf.area.height - 1)
            .map(|y| buf.get(right, y).symbol().to_string())
            .collect();
        assert!(
            sb_glyphs.iter().any(|g| g == "|" || g == "#"),
            "最右列应有 ASCII 滚动条: {sb_glyphs:?}"
        );
        // [v0.7] sparkline 行：有带时间戳的行时应有块字符
        // sparkline 行：工具栏下一行（含密度字符，日志行无 # 前缀）
        let spark_row = rs.iter().find(|r| {
            !r.contains("ERROR db error")
                && !r.contains("start sync")
                && (r.contains('*') || r.contains('#'))
                && !r.contains("┌")
        });
        assert!(spark_row.is_some(), "应有 ASCII sparkline 行: {all}");
    }

    #[test]
    fn 渲染_过滤命中计数与级别键生效() {
        let mut st = test_state();
        // 级别键 '1' → Error+：只剩 db error 行
        crate::ui::input::handle_normal_key(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('1'),
                crossterm::event::KeyModifiers::NONE,
            ),
            &mut st,
        );
        let t = draw(&mut st);
        let rs = rows(&t);
        let all = rs.join("\n");
        let f = flat(&all);
        assert!(all.contains("db error"), "Error 行可见: {all}");
        assert!(!all.contains("slow query"), "Warn 行被阈值滤掉: {all}");
        assert!(
            f.contains("3行1匹配"),
            "状态栏计数（buffer 3 行，阈值滤后 1 匹配）: {all}"
        );
    }

    #[test]
    fn 渲染_过滤文本反白高亮() {
        let mut st = test_state();
        st.filter_state = logtail_core::log::FilterState::Active(
            logtail_core::log::FilterMode::Text("refused".into()),
        );
        st.last_rebuild += 1;
        let t = draw(&mut st);
        let buf = t.backend().buffer();
        // 找到含 refused 的行，确认命中段单元格带 REVERSED
        let mut found = false;
        for y in 0..buf.area.height {
            let row: String = (0..buf.area.width)
                .map(|x| buf.get(x, y).symbol())
                .collect();
            // 只检查日志行（工具栏的 过滤:[refused] 回显不带反白）
            if row.contains("db error:") {
                let Some(byte_idx) = row.find("refused") else {
                    continue;
                };
                found = true;
                let x0 = row[..byte_idx].chars().count();
                for x in x0..x0 + "refused".len() {
                    assert!(
                        buf.get(x as u16, y)
                            .modifier
                            .contains(ratatui::style::Modifier::REVERSED),
                        "y={y} 列 {x} 应反白"
                    );
                }
            }
        }
        assert!(found, "应渲染出命中行");
    }

    #[test]
    fn 渲染_侧栏边框完整_无残留碎片() {
        // [v0.8.5] 布局代码正确性验证：侧栏有完整边框（┌─┐│）、无单字符碎片列
        let mut st = test_state();
        let t = draw(&mut st);
        let rs = rows(&t);
        // 侧栏框顶在 y=3（标题 3 行之下），col 0 应是 ┌；内容行 col 0 是 │
        assert_eq!(rs[3].chars().next().unwrap(), '┌', "侧栏框顶: {}", rs[3]);
        assert_eq!(rs[4].chars().next().unwrap(), '│', "侧栏左框: {}", rs[4]);
        assert_eq!(rs[4].chars().nth(23).unwrap(), '│', "侧栏右框: {}", rs[4]);
        // 全屏无单字符碎片行（旧 bug 特征：一行只有一个字符）
        for (i, r) in rs.iter().enumerate() {
            let visible = r.trim().chars().count();
            assert!(
                visible > 1 || r.trim().is_empty(),
                "y={i} 单字符碎片行: {r:?}"
            );
        }
    }

    #[test]
    fn 渲染_详情面板长行折行() {
        // [v0.10.5] 主视图撤换行（固定单行），详情面板 Paragraph 开 Wrap：
        // 长行在面板宽度内折行，尾部 TAIL 出现在面板区域内
        let mut st = test_state();
        let long = format!("HEAD-{}-TAIL", "x".repeat(80));
        let mut l = logtail_core::log::LogLine {
            raw: long,
            ..Default::default()
        };
        l.source = "test-src".into();
        if let Some(b) = st.buffers.get_mut("test-src") {
            b.push(l);
        }
        st.last_rebuild += 1;
        // test_state 预置 3 行；line_no 从 0 起编，长行（第 4 条）= 3
        st.detail_panel = Some(crate::app::DetailPanel {
            line_no: 3,
            context_before: 0,
            context_after: 0,
        });
        let t = draw(&mut st);
        let all = rows(&t).join("\n");
        assert!(all.contains("TAIL"), "详情面板折行后长行尾部应可见: {all}");
    }

    #[test]
    fn 渲染_多关键词全部反白() {
        // [v0.10] 'timeout|db' 两词都命中同一行 → 各自反白
        let mut st = test_state();
        let mut buf = logtail_core::log::LogBuffer::new(10);
        let mut l = logtail_core::log::LogLine {
            raw: "db error: timeout on connection".into(),
            level: logtail_core::log::LogLevel::Error,
            ..Default::default()
        };
        l.refresh_body(); // [v0.10.8] body 由 raw 派生（无时间戳前缀时 = raw）
        l.source = "test-src".into();
        buf.push(l);
        st.buffers.insert("test-src".into(), buf);
        st.filter_state = logtail_core::log::FilterState::Active(
            logtail_core::log::FilterMode::Text("timeout|db".into()),
        );
        st.last_rebuild += 1;
        let t = draw(&mut st);
        let buf = t.backend().buffer();
        // 找日志行，断言 "db" 与 "timeout" 两个片段都 REVERSED
        let mut db_rev = false;
        let mut to_rev = false;
        for y in 0..buf.area.height {
            let row: String = (0..buf.area.width)
                .map(|x| buf.get(x, y).symbol())
                .collect();
            if !row.contains("db error") {
                continue;
            }
            // [v0.10] row 每列一 symbol（char index == 列），find 的字节偏移须转列——
            // 侧栏含 CJK 时字节偏移 > 列（v0.8.5 教训的同族问题）
            let col_of = |b: usize| row[..b].chars().count();
            if let Some(b) = row.find("db") {
                let col = col_of(b) as u16;
                if buf
                    .get(col + 1, y)
                    .modifier
                    .contains(ratatui::style::Modifier::REVERSED)
                {
                    db_rev = true;
                }
            }
            if let Some(b) = row.find("timeout") {
                let col = col_of(b) as u16;
                if buf
                    .get(col + 1, y)
                    .modifier
                    .contains(ratatui::style::Modifier::REVERSED)
                {
                    to_rev = true;
                }
            }
        }
        assert!(
            db_rev && to_rev,
            "两个关键词都要反白 db_rev={db_rev} to_rev={to_rev}"
        );
        for y in 0..buf.area.height {
            let row: String = (0..buf.area.width)
                .map(|x| buf.get(x, y).symbol())
                .collect();
            if row.contains("db error") {
                let marks: String = (0..buf.area.width)
                    .map(|x| {
                        if buf
                            .get(x, y)
                            .modifier
                            .contains(ratatui::style::Modifier::REVERSED)
                        {
                            'R'
                        } else {
                            '.'
                        }
                    })
                    .collect();
                eprintln!("DBG|{row}");
                eprintln!("MRK|{marks}");
            }
        }
    }

    #[test]
    fn 渲染_相似行折叠() {
        // [v0.10] 连续相同消息体 → 1 行 + ×N；组内选中仍高亮代表行
        let mut st = test_state();
        let mut buf = logtail_core::log::LogBuffer::new(10);
        for i in 0..4 {
            let mut l = logtail_core::log::LogLine {
                raw: format!("2026-11-05T00:16:2{i}Z retry attempt"),
                ..Default::default()
            };
            l.refresh_body(); // [v0.10.8] 派生后剥时间戳：消息体相同 → 折叠
            l.source = "test-src".into();
            buf.push(l);
        }
        let mut diff = logtail_core::log::LogLine {
            raw: "different".into(),
            ..Default::default()
        };
        diff.refresh_body();
        buf.push(diff);
        st.buffers.insert("test-src".into(), buf);
        st.last_rebuild += 1;
        let t = draw(&mut st);
        let all = rows(&t).join(
            "
",
        );
        assert!(all.contains("retry attempt"), "折叠行显示消息体: {all}");
        assert!(all.contains("×4"), "行尾 ×4: {all}");
        assert!(all.contains("different"), "不同消息照常显示不折叠: {all}");
        // 关闭折叠：4 行全显
        st.ui.dedupe = false;
        st.last_rebuild += 1;
        let t = draw(&mut st);
        let all = rows(&t).join(
            "
",
        );
        assert_eq!(all.matches("retry attempt").count(), 4, "S 关闭后逐行显示");
    }

    #[test]
    fn 渲染_窄窗不溢出不碎片() {
        // [v0.8.5] 40x12 / 50x14（用户截图场景）/ 30x10：
        // 旧固定布局在此溢出——侧栏碎片/详情顶出屏幕/状态栏多行
        for (w, h) in [(40u16, 12u16), (50, 14), (30, 10)] {
            let mut st = test_state();
            st.detail_panel = Some(crate::app::DetailPanel {
                line_no: 1,
                context_before: 5,
                context_after: 5,
            });
            let backend = ratatui::backend::TestBackend::new(w, h);
            let mut t = ratatui::Terminal::new(backend).unwrap();
            t.draw(|f| render(f, &mut st)).unwrap();
            let buf = t.backend().buffer();
            let all: String = (0..buf.area.height)
                .flat_map(|y| (0..buf.area.width).map(move |x| (x, buf.get(x, y).symbol())))
                .map(|(_, s)| s)
                .collect();
            // 窄于最小尺寸 → 全屏提示
            if w < 40 || h < 12 {
                let flat: String = all.chars().filter(|c| !c.is_whitespace()).collect();
                assert!(flat.contains("终端太小"), "{w}x{h} 应显示提示: {all}");
                continue;
            }
            // 40x12：状态栏在最后一行（不被详情面板顶出屏幕）
            let last_row: String = (0..buf.area.width)
                .map(|x| buf.get(x, buf.area.height - 1).symbol())
                .collect();
            assert!(
                last_row.contains("行"),
                "{w}x{h} 最后一行应为状态栏: {last_row:?}"
            );
            // 侧栏区域（col 1..side_w）无日志内容污染
            let side_w = 24.min(w.saturating_sub(12)).max(10);
            for y in 0..buf.area.height {
                let row: String = (0..side_w.saturating_sub(1))
                    .map(|x| buf.get(x, y).symbol())
                    .collect();
                assert!(
                    !row.contains("zdhr"),
                    "{w}x{h} y={y} 侧栏被日志污染: {row:?}"
                );
            }
        }
    }

    #[test]
    fn 渲染_上翻后窗口跟随选中() {
        // [v0.7.6] 复现 bug①：旧渲染 scroll(scroll_y) 与多带的内容行抵消，
        // 光标上移屏幕不动。现在窗口精确切片——选中 L00 时日志区应显示头部行。
        let mut st = test_state();
        let mut buf = logtail_core::log::LogBuffer::new(100);
        for i in 0..30 {
            let mut l = logtail_core::log::LogLine {
                raw: format!("L{i:02}"),
                ..Default::default()
            };
            l.source = "test-src".into();
            l.refresh_body(); // 模拟 parse_line 填消息体（各行不同不折叠）
            buf.push(l);
        }
        st.buffers.insert("test-src".into(), buf);
        st.selected_line_no = Some(0); // 选中最顶行
        st.following = false;
        // 契约：scroll_y = len - visible + 选中下标(0) = 30 - 18 + 0 = 12
        // （真实场景由 move_selection 计算并写入，见 input.rs 键盘_上翻窗口跟随测试）
        st.viewport.scroll_y = 12;
        st.last_rebuild += 1;
        let t = draw(&mut st);
        let all = rows(&t).join(
            "
",
        );
        assert!(all.contains("L00"), "窗口必须滚到选中行: {all}");
        assert!(all.contains("L17"), "窗口 18 行到 L17");
        assert!(
            !all.contains("L29"),
            "末尾行不得残留（旧 bug 显示恒为末尾）"
        );
    }

    #[test]
    fn 渲染_倒序最新在顶() {
        // [v0.10.8] o 倒序：同一窗口切片反向渲染——跟随态顶行 = 最新（L29），
        // 窗口仍 18 行（切片公式两序等价，只反迭代）
        let mut st = test_state();
        let mut buf = logtail_core::log::LogBuffer::new(100);
        for i in 0..30 {
            let mut l = logtail_core::log::LogLine {
                raw: format!("L{i:02}"),
                ..Default::default()
            };
            l.source = "test-src".into();
            l.refresh_body(); // 各行不同不折叠
            buf.push(l);
        }
        st.buffers.insert("test-src".into(), buf);
        st.ui.reverse = true;
        st.last_rebuild += 1;
        let t = draw(&mut st);
        let all = rows(&t).join(
            "
",
        );
        assert!(all.contains("L29"), "倒序跟随态顶行应为最新 L29: {all}");
        assert!(all.contains("L12"), "窗口 18 行到 L12");
        assert!(!all.contains("L00"), "最旧行不得出现在跟随态: {all}");
    }

    #[test]
    fn 渲染_倒序_折叠组与行序() {
        // [v0.10.8] 倒序+折叠：等值游程对称分组不变（×4 保留）；最新行排屏幕最上
        let mut st = test_state();
        let mut buf = logtail_core::log::LogBuffer::new(10);
        for i in 0..4 {
            let mut l = logtail_core::log::LogLine {
                raw: format!("2026-11-05T00:16:2{i}Z retry attempt"),
                ..Default::default()
            };
            l.refresh_body(); // [v0.10.8] body 由 raw 派生
            l.source = "test-src".into();
            buf.push(l);
        }
        let mut diff = logtail_core::log::LogLine {
            raw: "different".into(),
            ..Default::default()
        };
        diff.refresh_body();
        buf.push(diff);
        st.buffers.insert("test-src".into(), buf);
        st.ui.reverse = true;
        st.last_rebuild += 1;
        let t = draw(&mut st);
        let all = rows(&t).join(
            "
",
        );
        assert!(all.contains("×4"), "倒序下折叠组仍 ×4: {all}");
        let pos_diff = all.find("different").unwrap();
        let pos_retry = all.find("retry attempt").unwrap();
        assert!(pos_diff < pos_retry, "倒序：最新行 different 在 retry 组上方: {all}");
    }

    #[test]
    fn 渲染_分屏双栏_各源各行() {
        // [v0.10.10] w 分屏：80x24 窄屏上下堆叠——两栏头 + 各自源的行都可见
        let mut st = test_state();
        let mut buf2 = logtail_core::log::LogBuffer::new(10);
        for tag in ["ALPHA-b", "BETA-b"] {
            let mut l = logtail_core::log::LogLine {
                raw: tag.into(),
                ..Default::default()
            };
            l.source = "src-b".into();
            l.refresh_body(); // 各行不同不折叠
            buf2.push(l);
        }
        st.buffers.insert("src-b".into(), buf2);
        st.tabs.push("src-b".into());
        st.toggle_split(); // 副栏取第一个非焦点 tab = src-b
        st.ui.show_sidebar = false;
        st.ui.show_sparkline = false;
        let t = draw(&mut st);
        let all = rows(&t).join(
            "
",
        );
        assert!(all.contains("[test-src]"), "焦点栏头: {all}");
        assert!(all.contains("[src-b]"), "副栏头: {all}");
        assert!(all.contains("start sync"), "焦点栏显示自己源的行: {all}");
        assert!(all.contains("ALPHA-b"), "副栏显示自己源的行: {all}");
    }

    #[test]
    fn 渲染_横向滚动_长行看全() {
        // [v0.7.6] scroll_x 平移：长行右侧内容随 ←/→ 可见
        let mut st = test_state();
        let mut buf = logtail_core::log::LogBuffer::new(10);
        let mut l = logtail_core::log::LogLine {
            raw: "AAAA".into(),
            ..Default::default()
        };
        l.refresh_body(); // [v0.10.8] body 由 raw 派生
        l.source = "test-src".into();
        buf.push(l);
        let mut l2 = logtail_core::log::LogLine {
            raw: "BBBB-TAIL".into(),
            ..Default::default()
        };
        l2.refresh_body();
        l2.source = "test-src".into();
        buf.push(l2);
        st.buffers.insert("test-src".into(), buf);
        st.viewport.scroll_x = 6; // 跳过 "INFO B" 前缀——行只有十几列，真实场景 ←/→ 步长 40
        st.last_rebuild += 1;
        let t = draw(&mut st);
        let all = rows(&t).join(
            "
",
        );
        assert!(
            all.contains("AIL"),
            "横滚后应看到长行尾部（skip 6 后剩 AIL）: {all}"
        );
        assert!(!all.contains("AAAA "), "前缀已被平移出屏: {all}");
    }

    #[test]
    fn 渲染_README界面帧快照() {
        // [v0.8.4] README 界面帧：双 tab（一流中一结束）+ 数据 + sparkline
        let mut st = test_state();
        st.tabs.push("ch-237".into());
        st.session_states
            .insert("ch-237".into(), crate::app::SessionState::Ended);
        let mut buf2 = logtail_core::log::LogBuffer::new(10);
        buf2.push(logtail_core::log::LogLine {
            raw: "2026-11-05T09:00:00Z ERROR on 237".into(),
            level: logtail_core::log::LogLevel::Error,
            ..Default::default()
        });
        st.buffers.insert("ch-237".into(), buf2);
        st.sources.insert(
            "ch-237".into(),
            logtail_core::config::SourceConfig {
                name: "ch-237".into(),
                server: "remote-237".into(),
                source: logtail_core::source::LogSource::Docker {
                    container: "c".into(),
                    tail: 10,
                    follow: true,
                    since: None,
                    until: None,
                },
            },
        );
        let mut cfg2 = st.config.clone();
        cfg2.servers.push(logtail_core::config::ServerConfig {
            name: "remote-237".into(),
            host: "192.168.40.237".into(),
            port: 22,
            user: "zdsoft".into(),
            auth: logtail_core::config::AuthCfg::Password {
                password: "x".into(),
            },
            sudo: false,
            sudo_password: None,
        });
        st.config = cfg2;
        for (raw, ago) in [
            ("2026-11-05T09:00:01Z 07:00:01 INFO sync start", 4i64),
            ("2026-11-05T09:00:02Z 07:00:02 ERROR db refused", 3),
            ("2026-11-05T09:00:03Z 07:00:03 WARN slow 1.2s", 2),
            ("2026-11-05T09:00:04Z 07:00:04 INFO done", 1),
        ] {
            let mut l = logtail_core::log::LogLine {
                raw: raw.into(),
                ..Default::default()
            };
            l.timestamp = Some(chrono::Utc::now() - chrono::Duration::seconds(ago));
            if let Some(b) = st.buffers.get_mut("test-src") {
                b.push(l);
            }
        }
        st.last_rebuild += 1;
        let t = draw(&mut st);
        let rs = rows(&t);
        for r in &rs {
            eprintln!("FRAME|{r}");
        }
        // sparkline 恢复断言：密度字符行存在（含 . 或 - 或 #），且非日志行
        let spark_ok = rs.iter().enumerate().any(|(i, r)| {
            i > 4
                && i < 7
                && (r.contains(".-") || r.contains("-#") || r.contains(":.") || r.ends_with('#'))
        });
        assert!(spark_ok, "sparkline 行必须渲染");
    }

    #[test]
    fn 渲染_详情面板只显示当前行() {
        let mut st = test_state();
        st.selected_line_no = Some(1);
        st.detail_panel = Some(crate::app::DetailPanel {
            line_no: 1,
            context_before: 5,
            context_after: 5,
        });
        let t = draw(&mut st);
        let all = rows(&t).join("\n");
        let f = flat(&all);
        assert!(f.contains("选中行详情"), "详情面板标题: {all}");
        assert!(f.contains("原始:dberror"), "原始行: {all}");
        // [v0.10.6] ±N 上下文撤销：不再有 ▶ 标记，标题无"（上下文±N）"标签
        assert!(!all.contains(">"), "上下文选中标记应已移除: {all}");
        assert!(!f.contains("（上下文±"), "标题不应再带上下文标签: {all}");
    }
}
