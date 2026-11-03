//! 渲染（§12.3 伪代码的落地）：标题 / 侧栏 / 工具栏 / 日志区 / 状态栏
//! （详情面板 Day6 接入；过滤高亮 Day4 接入）

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::AppState;
use crate::ui::layout::layout;

pub fn render(frame: &mut Frame, st: &mut AppState) {
    // [v0.4 P1#6] has_detail 从状态取
    let out = layout(frame.size(), st.detail_panel.is_some());

    // 日志区高度记录给输入层（↑↓ 保证选中行可见需要知道窗口大小）
    st.viewport.height = out.log.height.saturating_sub(2); // 减 Block 边框

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

fn render_sidebar(frame: &mut Frame, area: ratatui::layout::Rect, _st: &AppState) {
    // Day5：服务器树 + 源列表 + 预设（§12.1 左栏）
    let lines = vec![
        Line::from("服务器 / 源"),
        Line::from(Span::styled(
            "（Day5 接入配置）",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("源")),
        area,
    );
}

fn render_toolbar(frame: &mut Frame, area: ratatui::layout::Rect, st: &AppState) {
    let follow = if st.following { "● 跟随" } else { "‖ 暂停" };
    let line = Line::from(vec![Span::raw(format!("模式:全量   跟随:{follow}"))]);
    frame.render_widget(Paragraph::new(line), area);
}

fn render_logs(frame: &mut Frame, area: ratatui::layout::Rect, st: &AppState) {
    let visible = st.viewport.height as usize;
    // [v0.4 P2#3] scroll 是 render 期 builder（纵向偏移行, 横向偏移列）。
    // Paragraph 的 scroll 只会"向下翻"：上翻靠内容多带 scroll_y 行历史——
    // 内容 = 末尾 visible+scroll_y 行，scroll=(scroll_y,0) 恰显示 [len-visible-scroll_y, len-scroll_y)。
    let scroll_y = if st.following { 0 } else { st.viewport.scroll_y } as usize;
    let buf = st.current_buffer();
    let len = buf.len();
    let content_start = len.saturating_sub(visible + scroll_y);

    let mut lines = Vec::with_capacity(visible);
    for line in buf.iter().skip(content_start) {
        let base = Style::default().fg(line.level.color());
        let mut span = Span::styled(
            format!("{} {}", line.level.label(), line.raw),
            base,
        );
        if Some(line.line_no) == st.selected_line_no {
            span = span.style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD));
        }
        lines.push(Line::from(vec![span])); // 过滤关键词反白 Day4 接入
    }
    frame.render_widget(Paragraph::new(lines).scroll((scroll_y as u16, 0)), area);
}

fn render_statusbar(frame: &mut Frame, area: ratatui::layout::Rect, st: &AppState) {
    let hint = "q:退出 ↑↓:选择 Space:跟随 G:最新";
    let n = st.current_buffer().len();
    let line = Line::from(vec![
        Span::styled(hint, Style::default().fg(Color::DarkGray)),
        Span::raw(format!("  |  {}  |  {n} 行", st.status_msg)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}
