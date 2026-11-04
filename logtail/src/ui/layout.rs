//! Rect 切分（§12.2）

use ratatui::layout::{Constraint, Direction, Layout, Rect};

/// [v0.4 P1#6] has_detail 是形参——详情面板高度随选中动态。
pub fn layout(area: Rect, has_detail: bool) -> LayoutOut {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // 标题
            Constraint::Min(0),    // 主体
            Constraint::Length(1), // 状态栏
        ])
        .split(area);

    let body = chunks[1];
    let hchunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(24), // 左栏
            Constraint::Min(0),     // 右区
        ])
        .split(body);

    // 右区先分出工具栏一行（§12.1：工具栏是独立行，不能被日志覆盖），
    // 再分 sparkline 一行（[v0.7] 日志量时间轴）、日志区 / 详情面板
    let (toolbar, spark, log_area, detail_area) = if has_detail {
        let rchunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // 工具栏
                Constraint::Length(1), // sparkline [v0.7]
                Constraint::Min(8),    // 日志区
                Constraint::Length(16), // 详情面板 [v0.7.6] ±5 上下文全可见
            ])
            .split(hchunks[1]);
        (rchunks[0], rchunks[1], rchunks[2], Some(rchunks[3]))
    } else {
        let rchunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // 工具栏
                Constraint::Length(1), // sparkline [v0.7]
                Constraint::Min(0),    // 日志区
            ])
            .split(hchunks[1]);
        (rchunks[0], rchunks[1], rchunks[2], None)
    };

    LayoutOut {
        title: chunks[0],
        left: hchunks[0],
        toolbar,
        spark,
        log: log_area,
        detail: detail_area,
        statusbar: chunks[2],
    }
}

pub struct LayoutOut {
    pub title: Rect,
    pub left: Rect,
    pub toolbar: Rect,
    pub spark: Rect, // [v0.7] 日志量时间轴行
    pub log: Rect,
    pub detail: Option<Rect>,
    pub statusbar: Rect,
}
