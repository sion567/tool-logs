//! Rect 切分（§12.2）。[v0.8.5] 全部尺寸自适应 + 手动精确计算——
//! 固定约束（Length 24/16）在窄窗口下会被 ratatui solver 溢出绘制，
//! 把详情面板顶出屏幕、日志区压成单字符碎片（用户真机踩中）。
//! [v0.9] 侧栏/sparkline 显隐由 UiState 控制（b/v 热键）。

use ratatui::layout::Rect;

use crate::app::UiState;

/// [v0.8.5] 最小可用尺寸：低于此全屏提示（render 层守卫）
pub const MIN_WIDTH: u16 = 40;
pub const MIN_HEIGHT: u16 = 12;

/// [v0.8.5/v0.9] has_detail 必须是形参——详情面板高度随选中动态。
/// 所有分段手动计算精确适配 area，不再依赖约束求解器的溢出行为。
pub fn layout(area: Rect, has_detail: bool, ui: &UiState) -> LayoutOut {
    // 侧栏宽：固定 24，但窄屏让位给日志区（至少 10 列，日志区至少 12）；[v0.9] 可隐藏
    let side_w = if ui.show_sidebar {
        24.min(area.width.saturating_sub(12)).max(10)
    } else {
        0
    };

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

    // 主体水平切分（侧栏隐藏时日志区全宽）
    let left_w = side_w.min(body.width);
    let left = Rect { x: body.x, y: body.y, width: left_w, height: body.height };
    let right = Rect {
        x: body.x + left_w,
        y: body.y,
        width: body.width.saturating_sub(left_w),
        height: body.height,
    };

    // 右区垂直：工具栏 1 + [v0.9 sparkline 1 可隐藏] + 日志 X + 详情 D——手动精确适配
    let fixed = 1 + u16::from(ui.show_sparkline);
    // [v0.8.5→v0.9] 详情优先给 16 行，但日志区保底 4 行优先、详情至少 2 行
    let avail = body.height.saturating_sub(fixed);
    let (log_h, detail_area) = if has_detail {
        let log = 4.max(avail.saturating_sub(16)).min(avail.saturating_sub(2));
        let d = avail.saturating_sub(log);
        (
            log,
            Some(Rect {
                x: right.x,
                y: right.y + fixed + log,
                width: right.width,
                height: d,
            }),
        )
    } else {
        (avail, None)
    };
    let log_area = Rect {
        x: right.x,
        y: right.y + fixed,
        width: right.width,
        height: log_h,
    };
    let toolbar = Rect { x: right.x, y: right.y, width: right.width, height: 1 };
    let spark = Rect {
        x: right.x,
        y: right.y + 1,
        width: right.width,
        height: u16::from(ui.show_sparkline),
    };

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::UiState;

    #[test]
    fn layout_侧栏与sparkline显隐() {
        let area = Rect { x: 0, y: 0, width: 80, height: 24 };
        let mut ui = UiState::from_config(&logtail_core::config::UiCfg::default());
        ui.show_sparkline = false;
        let out = layout(area, false, &ui);
        assert_eq!(out.left.width, 24);
        assert_eq!(out.spark.height, 0, "sparkline 隐藏");
        assert_eq!(out.log.height, area.height - 3 - 1 - 1, "标题3+状态栏1+工具栏1之外全归日志");

        ui.show_sidebar = false;
        let out = layout(area, false, &ui);
        assert_eq!(out.left.width, 0, "侧栏隐藏");
        assert_eq!(out.log.width, area.width, "日志区全宽");

        // 溢出场景（40x12 + 详情）：日志区保底 4 行优先，详情压缩到剩余（至少 2 行）
        ui.show_sidebar = true;
        ui.show_sparkline = true;
        let small = Rect { x: 0, y: 0, width: 40, height: 12 };
        let out = layout(small, true, &ui);
        let d = out.detail.unwrap();
        assert_eq!(out.log.height, 4, "40x12 保底日志 4 行");
        assert_eq!(d.height, 2, "详情压缩到 2");
        assert!(d.height >= 2 && d.height <= 16, "详情在 2..=16 之间");
        assert_eq!(out.toolbar.height + out.spark.height + out.log.height + d.height, 8, "右区精确适配（body=12-3-1=8）");
        // 常规屏（24 行）+ 详情：详情优先 16，日志吃剩余（≥4）
        let out = layout(area, true, &ui);
        let d = out.detail.unwrap();
        assert_eq!(out.log.height + d.height, 18, "常规屏 body 20 - 工具栏1 - spark1 = 18");
        assert!(out.log.height >= 4);
    }
}
