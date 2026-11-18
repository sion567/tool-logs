//! 入口：默认 TUI（Day3 起）；`--demo` 保留无头打印模式做冒烟。

mod demo;

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("logtail-tui v{}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if let Some(pos) = args.iter().position(|a| a == "--check-config") {
        return check_config(args.get(pos + 1));
    }
    // [v0.10.9] keyring 子命令：密码写入/移出系统密钥环（§16.3），不进 TUI
    if args.len() >= 2 && args[1] == "keyring" {
        return keyring_cmd(&args[2..]);
    }
    if args.iter().any(|a| a == "--demo") {
        demo::run().await
    } else {
        logtail_core::config::append_log(&format!(
            "启动 logtail v{}（TUI，工作目录 {:?}）",
            env!("CARGO_PKG_VERSION"),
            std::env::current_dir()
                .map(|d| d.display().to_string())
                .unwrap_or_default()
        ));
        let result = match logtail_core::config::load() {
            Ok(config) => logtail_tui::app::run(config).await,
            Err(e) => {
                // [v0.7.1] 启动失败也落盘：双击闪退时终端一闪而过，日志里永远有痕
                logtail_core::config::append_log(&format!("启动失败: {e:#}"));
                eprintln!(
                    "启动失败（详情见 ~/.config/logtail/logtail.log）：
{e:#}"
                );
                eprintln!("提示：运行 --check-config 校验配置");
                Err(e)
            }
        };
        // [v0.8.2] 退出轨迹：闪退判据 = 有"启动"无"正常退出"
        logtail_core::config::append_log("正常退出");
        result
    }
}

/// [v0.7.1] --check-config：不进 TUI，校验配置并给出报告（连接失败/闪退先跑这个）
fn check_config(path: Option<&String>) -> Result<()> {
    let content = match path {
        Some(p) => std::fs::read_to_string(p)?,
        None => std::fs::read_to_string(logtail_core::config::config_dir().join("config.toml"))?,
    };
    let cfg: logtail_core::config::Config = toml::from_str(&content).map_err(|e| {
        anyhow::anyhow!(
            "TOML 解析失败: {e}
提示：字符串里的特殊字符用 \"\" 包裹；表格键不要重复"
        )
    })?;
    println!("✔ TOML 解析通过");
    println!(
        "✔ general: max_buffer_lines={}",
        cfg.general.max_buffer_lines
    );
    if cfg.servers.is_empty() {
        println!("⚠ 没有配置任何 [[servers]]——侧栏将无服务器可选");
    }
    for srv in &cfg.servers {
        // [v0.10.9] 认证方式按变体显示（明文时提示迁移密钥环）
        println!(
            "✔ server [{}]: {}@{}:{}（{}）",
            srv.name,
            srv.user,
            srv.host,
            srv.port,
            srv.auth.label()
        );
    }
    for src in &cfg.sources {
        let known = cfg.servers.iter().any(|s| s.name == src.server);
        if !known {
            println!(
                "✘ source [{}]: 引用的服务器 \"{}\" 不存在于 [[servers]]——连接必然失败",
                src.name, src.server
            );
        } else {
            println!(
                "✔ source [{}] → server [{}], {:?}",
                src.name,
                src.server,
                src.source.source_type()
            );
        }
    }
    let bad: Vec<_> = cfg
        .sources
        .iter()
        .filter(|s| !cfg.servers.iter().any(|x| x.name == s.server))
        .collect();
    if !bad.is_empty() {
        anyhow::bail!("{} 个源的 server 引用无效，修正后再启动", bad.len());
    }
    println!("✔ 配置整体有效，可以启动 ./logtail-tui");
    Ok(())
}

/// [v0.10.9] keyring 子命令：把服务器密码写入/移出系统密钥环（§16.3），不进 TUI。
/// 用法：`logtail-tui keyring set <服务器名>` / `logtail-tui keyring del <服务器名>`
fn keyring_cmd(rest: &[String]) -> Result<()> {
    match (rest.first().map(|s| s.as_str()), rest.get(1).map(|s| s.as_str())) {
        (Some("set"), Some(name)) => keyring_set(name),
        (Some("del") | Some("delete"), Some(name)) => keyring_del(name),
        _ => {
            eprintln!("用法: logtail-tui keyring set|del <服务器名>");
            eprintln!("  set  交互输入密码（隐藏回显，输两遍确认）写入系统密钥环");
            eprintln!("  del  从系统密钥环删除该服务器的条目");
            eprintln!("写入后把 config.toml 里该服务器的 auth 改为 {{ type = \"keyring\" }}");
            anyhow::bail!("缺少子命令或服务器名");
        }
    }
}

/// set：按 config 定位服务器（校验存在 + 取 keyring account 覆盖），交互隐藏输入
fn keyring_set(name: &str) -> Result<()> {
    let cfg = logtail_core::config::load()?;
    let srv = cfg.servers.iter().find(|s| s.name == name).ok_or_else(|| {
        let known: Vec<&str> = cfg.servers.iter().map(|s| s.name.as_str()).collect();
        if known.is_empty() {
            anyhow::anyhow!("config.toml 里没有服务器 [{name}]（尚未配置任何 [[servers]]）")
        } else {
            anyhow::anyhow!("config.toml 里没有服务器 [{name}]（现有: {}）", known.join(", "))
        }
    })?;
    // 账户名与 resolved_auth 的解析规则一致：显式 account 优先，缺省 = 服务器名；
    // 认证尚未切到 keyring 时也允许预存（先存密码、后改配置，顺序不敏感）
    let account = match &srv.auth {
        logtail_core::config::AuthCfg::Keyring { account } => {
            account.clone().unwrap_or_else(|| name.to_string())
        }
        _ => name.to_string(),
    };
    println!(
        "为服务器 [{name}]（{}@{}:{}）设置密钥环密码，账户: {account}",
        srv.user, srv.host, srv.port
    );
    let pw1 = rpassword::prompt_password("请输入密码（输入不回显）: ")?;
    if pw1.is_empty() {
        anyhow::bail!("密码不能为空");
    }
    let pw2 = rpassword::prompt_password("再输一遍确认: ")?;
    if pw1 != pw2 {
        anyhow::bail!("两次输入不一致，未写入");
    }
    logtail_core::config::keyring::set(&account, &pw1)?;
    println!("✔ 已写入系统密钥环（服务 {} / 账户 {account}）", logtail_core::config::keyring::SERVICE);
    println!("  现在可以把 config.toml 里该服务器的 auth 改为 {{ type = \"keyring\" }}");
    Ok(())
}

/// del：配置里已删掉该服务器也要能清凭据残留（查不到就按服务器名当账户名）
fn keyring_del(name: &str) -> Result<()> {
    let account = logtail_core::config::load()
        .ok()
        .and_then(|cfg| cfg.servers.into_iter().find(|s| s.name == name))
        .map(|srv| match srv.auth {
            logtail_core::config::AuthCfg::Keyring { account } => {
                account.unwrap_or(srv.name)
            }
            _ => srv.name,
        })
        .unwrap_or_else(|| name.to_string());
    logtail_core::config::keyring::delete(&account)?;
    println!("✔ 已从系统密钥环删除条目（账户 {account}）");
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
                .unwrap_or_else(|_| {
                    std::fs::File::create("logtail.log").expect("日志文件完全不可写")
                })
        })
        .try_init();
}
