//! docker 专用参数校验与命令构造（§6.2 docker 分支）

use super::{check_ident, check_time_value, shell_quote, ModeVariant};
use crate::error::LogtailError;

pub(super) fn docker_command(
    container: &str,
    tail: usize,
    follow: bool,
    since: Option<&String>,
    until: Option<&String>,
    mode: &ModeVariant,
) -> Result<String, LogtailError> {
    check_ident(container, "容器名")?;
    // [v0.4 P0#3 同源修法] docker 也直接 match mode 取时间窗：
    // Range 的 since 不经 mode.since()（它只在 Quick 有值），否则精准模式丢 --since
    let (m_since, m_until) = match mode {
        ModeVariant::Range(s, u) => (Some(s.as_str()), Some(u.as_str())),
        ModeVariant::Quick { since } => (since.as_deref(), None),
        ModeVariant::Full => (None, None),
    };
    let since_str = since.map(String::as_str).or(m_since); // 配置级 since 优先，mode 兜底
    let until_str = until.map(String::as_str).or(m_until);
    // [v0.4 P2#4] since/until 先校验再 quote（不变式③）；时间白名单含 : - . + 与空格
    if let Some(s) = since_str {
        check_time_value(s)?;
    }
    if let Some(u) = until_str {
        check_time_value(u)?;
    }
    let mut p = vec!["docker logs".to_string()];
    // -f 唯一决策者：Range 恒不带；Full/Quick 看源声明（§10 唯一真理）
    if mode.follow() && follow {
        p.push("-f".to_string());
    }
    p.push(format!("--tail {tail}"));
    if let Some(s) = since_str {
        p.push(format!("--since {}", shell_quote(s)));
    }
    if let Some(u) = until_str {
        p.push(format!("--until {}", shell_quote(u)));
    }
    p.push("--timestamps".to_string());
    p.push(shell_quote(container));
    Ok(p.join(" "))
}
