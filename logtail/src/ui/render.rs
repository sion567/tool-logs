//! 渲染（§12.3 伪代码的落地）：标题 / 侧栏 / 工具栏 / 日志区 / 状态栏
//! （详情面板 Day6 接入）

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::AppState;
use crate::log::{rebuild_view, FilterMode};
use crate::ui::layout::layout;

pub fn render(frame: &mut Frame, st: &mut AppState) {
    // [v0.4 P1#6] has_detail 从状态取
    let out = layout(frame.size(), st.detail_panel.is_some());

    // 日志区高度记录给输入层（↑↓ 保证选中行可见需要知道窗口大小）
    st.viewport.height = out.log.height.saturating_sub(2); // 减 Block 边框

    // [v0.4 P2#2] 命中缓存：仅 last_rebuild 变化（入行/过滤变更）才全量重扫，其余帧直接复用
    if st.view_cache.built_at != st.last_rebuild {
        let hits = rebuild_view(st.current_buffer(), st.active_filter(), st.level_filter);
        st.view_cache = crate::app::ViewCache { built_at: st.last_rebuild, hits };
    }

    render_title(frame, out.title, st);
    render_sidebar(frame, out.left, st);
    render_toolbar(frame, out.toolbar, st);
    render_logs(frame, out.log, st);
    render_statusbar(frame, out.statusbar, st);
}

fn render_title(frame: &mut Frame, area: ratatui::layout::Rect, st: &AppState) {
    let src = st.active_source.clone().unwrap_or_else(|| "未连接".into());
    let line = Line::from(vec![
        Span::styled(
            "远程日志查看器 v0.4.0",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("   ● {src}")),
    ]);
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
            crate::app::SideEntry::Server(_) => {
                Line::from(Span::styled(label.clone(), Style::default().add_modifier(Modifier::BOLD)))
            }
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
    let follow = if st.following { "● 跟随" } else { "‖ 暂停" };
    let regex = if st.regex_mode { "正则:开" } else { "正则:关" };
    // 过滤框显示（§12.1）：Typing 显示输入+光标；Error 红色提示；其余显示生效过滤
    let (filter_label, filter_style) = match &st.filter_state {
        crate::log::FilterState::Typing { input, cursor } => {
            (render_input(input, *cursor), Style::default().fg(Color::Yellow))
        }
        crate::log::FilterState::Error(_) => (
            "!正则错误,Esc/Enter 继续编辑".into(),
            Style::default().fg(Color::Red),
        ),
        _ => {
            let t = match st.active_filter() {
                FilterMode::None => "无".to_string(),
                FilterMode::Text(t) => t.clone(),
                FilterMode::Regex(_, t) => format!("re:{t}"),
            };
            (t, Style::default())
        }
    };
    let line = Line::from(vec![
        Span::raw("模式:"),
        Span::styled(st.mode.label(), Style::default().fg(Color::Magenta)),
        Span::raw("  级别:"),
        Span::styled(st.level_filter.label(), Style::default().fg(Color::Cyan)),
        Span::raw("  过滤:["),
        Span::styled(filter_label, filter_style),
        Span::raw("]  "),
        Span::raw(regex),
        Span::raw(format!("  跟随:{follow}")),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

/// Typing 输入框：▌ 在光标字符位（字符计数）
fn render_input(input: &str, cursor: usize) -> String {
    let chars: Vec<char> = input.chars().collect();
    let c = cursor.min(chars.len());
    let mut s = String::new();
    for (i, ch) in chars.iter().enumerate() {
        if i == c {
            s.push('▌');
        }
        s.push(*ch);
    }
    if c >= chars.len() {
        s.push('▌');
    }
    s
}

fn render_logs(frame: &mut Frame, area: ratatui::layout::Rect, st: &AppState) {
    let visible = st.viewport.height as usize;
    // [v0.4 P2#3] scroll 是 render 期 builder（纵向偏移行, 横向偏移列）。
    // Paragraph 的 scroll 只会"向下翻"：上翻靠内容多带 scroll_y 行历史——
    // 内容 = 末尾 visible+scroll_y 行，scroll=(scroll_y,0) 恰显示 [len-visible-scroll_y, len-scroll_y)。
    let scroll_y = if st.following { 0 } else { st.viewport.scroll_y } as usize;
    let hits = &st.view_cache.hits;
    let len = hits.len();
    let content_start = len.saturating_sub(visible + scroll_y);

    let fm = st.active_filter().clone();
    let mut lines = Vec::with_capacity(visible);
    for &no in &hits[content_start..] {
        let line = match st.current_buffer().by_line_no(no) {
            Some(l) => l,
            None => continue, // 已淘汰：缓存比 buffer 旧一拍，下帧重建
        };
        let selected = Some(no) == st.selected_line_no;
        let base = Style::default().fg(line.level.color());
        let mut spans = vec![Span::styled(
            format!("{} ", line.level.label()),
            if selected {
                base.bg(Color::DarkGray).add_modifier(Modifier::BOLD)
            } else {
                base
            },
        )];
        // 关键词反白（过滤文本高亮，§12.3）
        for (seg, matched) in split_matches(&line.raw, &fm) {
            let style = match (matched, selected) {
                (true, true) => base.bg(Color::DarkGray).add_modifier(Modifier::REVERSED | Modifier::BOLD),
                (true, false) => base.add_modifier(Modifier::REVERSED),
                (false, true) => base.bg(Color::DarkGray).add_modifier(Modifier::BOLD),
                (false, false) => base,
            };
            spans.push(Span::styled(seg.to_string(), style));
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines).scroll((scroll_y as u16, 0)), area);
}

/// 按 FilterMode 把原文切成 (片段, 是否命中) 序列，用于反白高亮
fn split_matches<'a>(text: &'a str, fm: &FilterMode) -> Vec<(&'a str, bool)> {
    match fm {
        FilterMode::None => vec![(text, false)],
        FilterMode::Text(t) => {
            if t.is_empty() {
                return vec![(text, false)];
            }
            let mut out = Vec::new();
            let mut rest = text;
            while let Some(pos) = rest.find(t) {
                if pos > 0 {
                    out.push((&rest[..pos], false));
                }
                out.push((&rest[pos..pos + t.len()], true));
                rest = &rest[pos + t.len()..];
            }
            if !rest.is_empty() {
                out.push((rest, false));
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
    let hint = "q:退出 /:过滤 r:正则 1-4:级别 Space:跟随 G:最新";
    let (n, hits) = (st.current_buffer().len(), st.view_cache.hits.len());
    let line = Line::from(vec![
        Span::styled(hint, Style::default().fg(Color::DarkGray)),
        Span::raw(format!("  |  {}  |  {n} 行 {hits} 匹配", st.status_msg)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}
