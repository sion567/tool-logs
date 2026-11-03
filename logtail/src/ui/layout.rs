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

    // 右区再分：日志区 / 详情面板（详情高度随选中动态）
    let (log_area, detail_area) = if has_detail {
        let rchunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(8),     // 日志区
                Constraint::Length(8),  // 详情面板
            ])
            .split(hchunks[1]);
        (rchunks[0], Some(rchunks[1]))
    } else {
        (hchunks[1], None)
    };

    LayoutOut {
        title: chunks[0],
        left: hchunks[0],
        toolbar: log_area, // 工具栏叠加在日志区顶部一行
        log: log_area,
        detail: detail_area,
        statusbar: chunks[2],
    }
}

pub struct LayoutOut {
    pub title: Rect,
    pub left: Rect,
    pub toolbar: Rect,
    pub log: Rect,
    pub detail: Option<Rect>,
    pub statusbar: Rect,
}
