//! 渲染（§12.3 伪代码的落地）：标题 / 侧栏 / 工具栏 / 日志区 / 状态栏
//! （详情面板 Day6 接入）

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use ratatui::Frame;

use crate::app::{AppState, SessionState};
use crate::log::{rebuild_view, FilterMode};
use crate::ui::layout::layout;

pub fn render(frame: &mut Frame, st: &mut AppState) {
    // [v0.4 P1#6] has_detail 从状态取
    let out = layout(frame.size(), st.detail_panel.is_some());

    // 日志区高度记录给输入层（↑↓ 保证选中行可见需要知道窗口大小）
    st.viewport.height = out.log.height;

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
        st.view_cache = crate::app::ViewCache { built_at: st.last_rebuild, hits, spark };
    }

    render_title(frame, out.title, st);
    render_sidebar(frame, out.left, st);
    render_toolbar(frame, out.toolbar, st);
    render_logs(frame, out.log, st);

    // 详情面板（Day6）：时间/级别/源/原始行/上下文±N
    if let (Some(rect), Some(panel)) = (out.detail, st.detail_panel.clone()) {
        render_detail(frame, rect, st, &panel);
    }
    render_statusbar(frame, out.statusbar, st);
}

fn render_title(frame: &mut Frame, area: ratatui::layout::Rect, st: &AppState) {
    // [v0.5] 标题栏放 tab 条：[源名] 按连接顺序排列，激活 tab 反白高亮，
    // 每个带会话状态符（●流中 ⟳连接中 ■结束 ✗失败）
    let mut spans = vec![Span::styled(
        format!("远程日志查看器 v{}", env!("CARGO_PKG_VERSION")),
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
    )];
    if st.tabs.is_empty() {
        spans.push(Span::raw("   未连接（Tab 聚焦侧栏选源回车）"));
    }
    for tab in &st.tabs {
        let marker = match st.session_states.get(tab).copied() {
            Some(SessionState::Connecting) => "⟳",
            Some(SessionState::Streaming) => "●",
            Some(SessionState::Ended) => "■",
            Some(SessionState::Failed) => "✗",
            None => " ",
        };
        let active = st.active_source.as_deref() == Some(tab.as_str());
        let label = format!(" {marker}[{tab}] ");
        spans.push(Span::styled(
            label,
            if active {
                Style::default().bg(Color::Blue).fg(Color::White).add_modifier(Modifier::BOLD)
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
            Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
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
    // 位置滚动条（§12.1 / Day3 任务清单）：thumb 表示当前视口在 hits 中的位置
    let mut sb_state = ScrollbarState::new(len).position(content_start);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight),
        area,
        &mut sb_state,
    );
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
    let hint = "Tab:侧栏 q:退出 /:过滤 ^R:正则 f:模式 R:重连 L:更早 e:导出 P:远筛 C:清屏 1-4:级别 Space:跟随 G:最新";
    let (n, hits) = (st.current_buffer().len(), st.view_cache.hits.len());
    // 断网/认证失败/超时 → 红字（§四 Day7），正常消息灰底黑字
    let failed = st
        .status_msg
        .contains("失败")
        || st.status_msg.contains("超时");
    let msg_span = if failed {
        Span::styled(st.status_msg.clone(), Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
    } else {
        Span::raw(st.status_msg.clone())
    };
    // 计数放最前：窄终端截断时保住关键信息（提示语牺牲尾部）
    // [v0.7.1] 连接正常但零日志：主动给出原因提示（最常见的"没数据"是 --since 滤掉了历史）
    let mut extra = String::new();
    if n == 0 && st.session_states.get(st.active_source.as_deref().unwrap_or("")) == Some(&crate::app::SessionState::Streaming) {
        extra = match &st.mode {
            crate::source::ModeVariant::Quick { since: Some(s) } => format!(
                "  |  ⚠ 当前只回放最近 {s}，期间无日志。按 f 切全量回放全部历史，或 L 前移"
            ),
            _ => "  |  ⚠ 已连接但暂无日志，等待新输出…（按 L 回放更早）".into(),
        };
    }
    let line = Line::from(vec![
        Span::raw(format!("{n} 行 {hits} 匹配")),
        Span::raw("  |  "),
        msg_span,
        Span::raw(format!("{extra}  |  ")),
        Span::styled(hint, Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

/// 详情面板（§12.1 底部）：时间 / 级别 / 源 / 原始行 / 上下文±N / 操作提示
fn render_detail(frame: &mut Frame, area: ratatui::layout::Rect, st: &AppState, panel: &crate::app::DetailPanel) {
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
            Span::styled(line.level.label().trim(), Style::default().fg(line.level.color())),
            Span::raw(format!("   源: {}", if line.source.is_empty() { "-" } else { &line.source })),
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
        lines.push(Line::from(Span::styled(
            format!(
                "上下文 ±{}（{} - {}）:",
                panel.context_after, panel.context_before, panel.context_after
            ),
            Style::default().fg(Color::DarkGray),
        )));
        // 上下文窗口：选中行标 ▶（Day6 验收）
        for cl in buf.context(panel.line_no, panel.context_before, panel.context_after) {
            let marker = if cl.line_no == panel.line_no { "▶" } else { " " };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{marker} "),
                    Style::default().fg(if cl.line_no == panel.line_no { Color::Yellow } else { Color::DarkGray }),
                ),
                Span::styled(cl.raw.clone(), Style::default().fg(cl.level.color())),
            ]));
        }
    } else {
        lines.push(Line::from(Span::styled(
            "该行已被淘汰（buffer 窗口外）",
            Style::default().fg(Color::Red),
        )));
    }
    lines.push(Line::from(Span::styled(
        "q:关闭 c:复制 t:trace过滤 p:聚焦源",
        Style::default().fg(Color::DarkGray),
    )));

    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("选中行详情 (line_no={})", panel.line_no)),
        ),
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
        let mut cfg = crate::config::Config::default();
        cfg.general.remember_last_source = false;
        let mut st = AppState::new(cfg);
        let mut buf = crate::log::LogBuffer::new(100);
        let now = chrono::Utc::now();
        let rows = [
            (crate::log::LogLevel::Info, "start sync", 2i64),
            (crate::log::LogLevel::Error, "db error: connection refused", 0),
            (crate::log::LogLevel::Warn, "slow query 1.2s", 0),
        ];
        for (lvl, raw, ago) in rows {
            let mut line = crate::log::LogLine { level: lvl, raw: raw.into(), ..Default::default() };
            line.source = "test-src".into();
            line.timestamp = Some(now - chrono::Duration::seconds(ago));
            buf.push(line);
        }
        st.buffers.insert("test-src".into(), buf);
        st.sources.insert(
            "test-src".into(),
            crate::config::SourceConfig {
                name: "test-src".into(),
                server: "local".into(),
                source: crate::source::LogSource::File { path: "/var/log/x.log".into(), tail: 10 },
            },
        );
        st.active_source = Some("test-src".into());
        st.tabs.push("test-src".into()); // [v0.5] tab 条需要已连接列表
        st.session_states.insert("test-src".into(), crate::app::SessionState::Streaming);
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
        assert!(all.contains("db error: connection refused"), "日志行: {all}");
        assert!(f.contains("3行3匹配"), "状态栏计数: {all}");
        // 滚动条（Day3 任务项）：日志区最右列应有轨道/拇指符号
        let buf = t.backend().buffer();
        let right = buf.area.width - 1;
        let sb_glyphs: Vec<String> = (4..buf.area.height - 1)
            .map(|y| buf.get(right, y).symbol().to_string())
            .collect();
        assert!(
            sb_glyphs.iter().any(|g| g == "│" || g == "█" || g == "▲" || g == "▼"),
            "最右列应有滚动条: {sb_glyphs:?}"
        );
        // [v0.7] sparkline 行：有带时间戳的行时应有块字符
        let spark_row = rs
            .iter()
            .find(|r| r.contains('▁') || r.contains('▄') || r.contains('█'));
        assert!(spark_row.is_some(), "应有 sparkline 行: {all}");
    }

    #[test]
    fn 渲染_过滤命中计数与级别键生效() {
        let mut st = test_state();
        // 级别键 '1' → Error+：只剩 db error 行
        crate::ui::input::handle_normal_key(
            crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Char('1'), crossterm::event::KeyModifiers::NONE),
            &mut st,
        );
        let t = draw(&mut st);
        let rs = rows(&t);
        let all = rs.join("\n");
        let f = flat(&all);
        assert!(all.contains("db error"), "Error 行可见: {all}");
        assert!(!all.contains("slow query"), "Warn 行被阈值滤掉: {all}");
        assert!(f.contains("3行1匹配"), "状态栏计数（buffer 3 行，阈值滤后 1 匹配）: {all}");
    }

    #[test]
    fn 渲染_过滤文本反白高亮() {
        let mut st = test_state();
        st.filter_state = crate::log::FilterState::Active(crate::log::FilterMode::Text("refused".into()));
        st.last_rebuild += 1;
        let t = draw(&mut st);
        let buf = t.backend().buffer();
        // 找到含 refused 的行，确认命中段单元格带 REVERSED
        let mut found = false;
        for y in 0..buf.area.height {
            let row: String = (0..buf.area.width).map(|x| buf.get(x, y).symbol()).collect();
            // 只检查日志行（工具栏的 过滤:[refused] 回显不带反白）
            if row.contains("db error:") {
                let Some(byte_idx) = row.find("refused") else { continue };
                found = true;
                let x0 = row[..byte_idx].chars().count();
                for x in x0..x0 + "refused".len() {
                    assert!(
                        buf.get(x as u16, y).modifier.contains(ratatui::style::Modifier::REVERSED),
                        "y={y} 列 {x} 应反白"
                    );
                }
            }
        }
        assert!(found, "应渲染出命中行");
    }

    #[test]
    fn 渲染_详情面板与上下文标记() {
        let mut st = test_state();
        st.selected_line_no = Some(1);
        st.detail_panel = Some(crate::app::DetailPanel { line_no: 1, context_before: 5, context_after: 5 });
        let t = draw(&mut st);
        let all = rows(&t).join("\n");
        let f = flat(&all);
        assert!(f.contains("选中行详情"), "详情面板标题: {all}");
        assert!(all.contains("▶"), "上下文选中标记: {all}");
        assert!(f.contains("原始:dberror"), "原始行: {all}");
    }
}
