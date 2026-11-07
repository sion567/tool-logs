//! logtail-gui — BareTail 风格的图形界面日志查看器。
//!
//! 复用 logtail 库：本地 tail 泵（轮转自愈）/ SSH 连接 / 行解析 / 过滤 / 缓冲。
//! 入口职责：字体注入（中文显示）、窗口参数、CLI 文件参数转交 App。

mod prefs;
mod pump;
mod remote;
mod tab;
mod ui;

use eframe::egui::{Context, FontData, FontDefinitions, FontFamily, ViewportBuilder};

fn main() -> eframe::Result {
    let prefs = prefs::Prefs::load();
    // CLI 参数：存在的文件直接开 tab（logtail-gui.exe a.log b.log）
    let files: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| std::path::Path::new(a).is_file())
        .collect();

    let mut options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_inner_size([prefs.window.w.max(400.0), prefs.window.h.max(300.0)])
            .with_title("logtail-gui — 日志实时监控"),
        ..Default::default()
    };
    configure_windows_backends(&mut options);

    eframe::run_native(
        "logtail-gui",
        options,
        Box::new(move |cc| {
            let font_ok = install_cjk_fonts(&cc.egui_ctx);
            // BareTail 白底风格：固定浅色主题（不跟随系统深浅色）
            cc.egui_ctx.set_visuals(eframe::egui::Visuals::light());
            Ok(Box::new(ui::LogTailApp::new(prefs, files, font_ok)))
        }),
    )
}

/// Windows 渲染后端白名单：默认禁用 Vulkan。
/// 实测部分 Windows 机器（含本机）wgpu 走 Vulkan 直接 0xC0000005 闪退；
/// DX12 是 Windows 原生路径（优先），GL 兜底。仍可用 WGPU_BACKEND 环境变量覆盖。
#[cfg(target_os = "windows")]
fn configure_windows_backends(options: &mut eframe::NativeOptions) {
    use egui_wgpu::{wgpu, WgpuConfiguration, WgpuSetup, WgpuSetupCreateNew};
    let backends = wgpu::Backends::from_env()
        .unwrap_or(wgpu::Backends::DX12 | wgpu::Backends::GL);
    options.wgpu_options = WgpuConfiguration {
        wgpu_setup: WgpuSetup::CreateNew(WgpuSetupCreateNew {
            instance_descriptor: wgpu::InstanceDescriptor {
                backends,
                flags: wgpu::InstanceFlags::from_build_config().with_env(),
                backend_options: wgpu::BackendOptions::from_env_or_default(),
                memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
                display: None,
            },
            ..WgpuSetupCreateNew::without_display_handle()
        }),
        ..WgpuConfiguration::default()
    };
}

#[cfg(not(target_os = "windows"))]
fn configure_windows_backends(_options: &mut eframe::NativeOptions) {}

/// 注入中文字体：egui 内置字体无 CJK 字形，缺了中文全是豆腐块。
/// 依序找 Windows 常见中文字体，注入到 Proportional 与 Monospace 两个 family
/// （等宽家族 ASCII 仍用内置等宽，中文回退到系统字体）。
fn install_cjk_fonts(ctx: &Context) -> bool {
    const CANDIDATES: &[&str] = &[
        "C:/Windows/Fonts/msyh.ttc",   // 微软雅黑
        "C:/Windows/Fonts/msyh.ttf",
        "C:/Windows/Fonts/msyhbd.ttc",
        "C:/Windows/Fonts/simhei.ttf", // 黑体
        "C:/Windows/Fonts/simsun.ttc", // 宋体
        "C:/Windows/Fonts/Deng.ttf",   // 等线
    ];
    for path in CANDIDATES {
        let Ok(bytes) = std::fs::read(path) else { continue };
        let mut fonts = FontDefinitions::default();
        fonts
            .font_data
            .insert("cjk".into(), std::sync::Arc::new(FontData::from_owned(bytes)));
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push("cjk".into());
        }
        ctx.set_fonts(fonts);
        return true;
    }
    false
}
