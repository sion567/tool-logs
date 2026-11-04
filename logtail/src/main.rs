//! 入口：默认 TUI（Day3 起）；`--demo` 保留无头打印模式做冒烟。

mod demo;

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("logtail v{}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if let Some(pos) = args.iter().position(|a| a == "--check-config") {
        return check_config(args.get(pos + 1));
    }
    if args.iter().any(|a| a == "--demo") {
        demo::run().await
    } else {
        logtail::config::append_log(&format!(
            "启动 logtail v{}（TUI，工作目录 {:?}）",
            env!("CARGO_PKG_VERSION"),
            std::env::current_dir().map(|d| d.display().to_string()).unwrap_or_default()
        ));
        let result = match logtail::config::load() {
            Ok(config) => logtail::app::run(config).await,
            Err(e) => {
                // [v0.7.1] 启动失败也落盘：双击闪退时终端一闪而过，日志里永远有痕
                logtail::config::append_log(&format!("启动失败: {e:#}"));
                eprintln!("启动失败（详情见 ~/.config/logtail/logtail.log）：
{e:#}");
                eprintln!("提示：运行 --check-config 校验配置");
                Err(e)
            }
        };
        // [v0.8.2] 退出轨迹：闪退判据 = 有"启动"无"正常退出"
        logtail::config::append_log("正常退出");
        result
    }
}

/// [v0.7.1] --check-config：不进 TUI，校验配置并给出报告（连接失败/闪退先跑这个）
fn check_config(path: Option<&String>) -> Result<()> {
    let content = match path {
        Some(p) => std::fs::read_to_string(p)?,
        None => std::fs::read_to_string(logtail::config::config_dir().join("config.toml"))?,
    };
    let cfg: logtail::config::Config = toml::from_str(&content).map_err(|e| {
        anyhow::anyhow!("TOML 解析失败: {e}
提示：字符串里的特殊字符用 \"\" 包裹；表格键不要重复")
    })?;
    println!("✔ TOML 解析通过");
    println!("✔ general: max_buffer_lines={}", cfg.general.max_buffer_lines);
    if cfg.servers.is_empty() {
        println!("⚠ 没有配置任何 [[servers]]——侧栏将无服务器可选");
    }
    for srv in &cfg.servers {
        println!("✔ server [{}]: {}@{}:{}（密码认证）", srv.name, srv.user, srv.host, srv.port);
    }
    for src in &cfg.sources {
        let known = cfg.servers.iter().any(|s| s.name == src.server);
        if !known {
            println!("✘ source [{}]: 引用的服务器 \"{}\" 不存在于 [[servers]]——连接必然失败", src.name, src.server);
        } else {
            println!("✔ source [{}] → server [{}], {:?}", src.name, src.server, src.source.source_type());
        }
    }
    let bad: Vec<_> = cfg.sources.iter().filter(|s| !cfg.servers.iter().any(|x| x.name == s.server)).collect();
    if !bad.is_empty() {
        anyhow::bail!("{} 个源的 server 引用无效，修正后再启动", bad.len());
    }
    println!("✔ 配置整体有效，可以启动 ./logtail");
    Ok(())
}

/// 自身排障日志（§2）：设 LOGTAIL_LOG=1 时写 ./logtail.log（绝不污染 TUI stdout）
fn init_tracing() {
    if std::env::var("LOGTAIL_LOG").is_err() {
        return;
    }
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        // [v0.8.2] 打开失败降级为无日志，不 panic（曾是可以闪退的 expect）
        .with_writer(|| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open("logtail.log")
                .unwrap_or_else(|_| std::fs::File::create("logtail.log").expect("日志文件完全不可写"))
        })
        .try_init();
}
