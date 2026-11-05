//! Rect 切分（§12.2）。[v0.8.5] 全部尺寸自适应 + 手动精确计算——
//! 固定约束（Length 24/16）在窄窗口下会被 ratatui solver 溢出绘制，
//! 把详情面板顶出屏幕、日志区压成单字符碎片（用户真机踩中）。

use ratatui::layout::Rect;

/// [v0.8.5] 最小可用尺寸：低于此全屏提示（render 层守卫）
pub const MIN_WIDTH: u16 = 40;
pub const MIN_HEIGHT: u16 = 12;

/// [v0.8.5] has_detail 必须是形参——详情面板高度随选中动态。
/// 所有分段手动计算精确适配 area，不再依赖约束求解器的溢出行为。
pub fn layout(area: Rect, has_detail: bool) -> LayoutOut {
    // 侧栏宽：固定 24，但窄屏让位给日志区（至少 10 列，日志区至少 12）
    let side_w = 24.min(area.width.saturating_sub(12)).max(10);
    
    // 垂直：标题 3 + 主体 + 状态栏 1
    let title_h = 3.min(area.height);
    let title = Rect { x: area.x, y: area.y, width: area.width, height: title_h };
    let statusbar = Rect {
        x: area.x,
        y: area.y + area.height.saturating_sub(1),
        width: area.width,
        height: 1.min(area.height),
    };
    let body = Rect {
        x: area.x,
        y: area.y + title_h,
        width: area.width,
        height: area.height.saturating_sub(title_h + 1),
    };

    // 主体水平切分
    let left_w = side_w.min(body.width);
    let left = Rect { x: body.x, y: body.y, width: left_w, height: body.height };
    let right = Rect {
        x: body.x + left_w,
        y: body.y,
        width: body.width.saturating_sub(left_w),
        height: body.height,
    };

    // 右区垂直：工具栏 1 + sparkline 1 + 日志 X + 详情 D——手动精确适配
    let fixed = 2u16; // 工具栏 + sparkline
    let (log_h, _detail_h, detail_area) = if has_detail {
        // 详情最多 16 行，但保证日志区至少 6 行：D = min(16, body-fixed-6)
        let d = 16.min(body.height.saturating_sub(fixed + 6)).max(4);
        let l = body.height.saturating_sub(fixed + d);
        (
            l,
            d,
            Some(Rect {
                x: right.x,
                y: right.y + fixed + l,
                width: right.width,
                height: d,
            }),
        )
    } else {
        (body.height.saturating_sub(fixed), 0, None)
    };
    let log_area = Rect {
        x: right.x,
        y: right.y + fixed,
        width: right.width,
        height: log_h,
    };
    let toolbar = Rect { x: right.x, y: right.y, width: right.width, height: 1 };
    let spark = Rect { x: right.x, y: right.y + 1, width: right.width, height: 1 };

    LayoutOut {
        title,
        left,
        toolbar,
        spark,
        log: log_area,
        detail: detail_area,
        statusbar,
    }
}

pub struct LayoutOut {
    pub title: Rect,
    pub left: Rect,
    pub toolbar: Rect,
    pub spark: Rect,
    pub log: Rect,
    pub detail: Option<Rect>,
    pub statusbar: Rect,
}
