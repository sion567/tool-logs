//! journalctl 相对时长换算（§6.4）与命令构造

use chrono::Utc;

use super::{check_ident, check_time_value, shell_quote, ModeVariant};
use crate::error::LogtailError;

/// journalctl --since 不接受 "1h"/"30m"，只认绝对时间或 today/yesterday。
/// 相对时长 → UTC RFC3339；已是绝对格式则原样返回。
pub fn parse_journal_since(s: &str) -> String {
    let re = regex::Regex::new(r"^(\d+)([hmd])$").unwrap();
    let Some(caps) = re.captures(s) else {
        return s.to_string();
    };
    let n: i64 = caps[1].parse().unwrap_or(1);
    let d = match &caps[2] {
        "h" => chrono::Duration::hours(n),
        "d" => chrono::Duration::days(n),
        "m" => chrono::Duration::minutes(n),
        _ => chrono::Duration::hours(1),
    };
    (Utc::now() - d).to_rfc3339()
}

/// journalctl 分支（§6.2 [v0.4 P0#3]）：直接 match mode 拿时间窗，不绕 mode.since()。
pub(super) fn journal_command(
    service: &str,
    since: Option<&String>,
    mode: &ModeVariant,
) -> Result<String, LogtailError> {
    check_ident(service, "服务名")?;
    let (mode_since, mode_until) = match mode {
        ModeVariant::Range(s, u) => (Some(s), Some(u)),
        ModeVariant::Quick { since } => (since.as_ref(), None),
        ModeVariant::Full => (None, None),
    };
    let since_str = mode_since.or(since); // 配置级 since 仅兜底
    if let Some(s) = since_str {
        check_time_value(s)?;
    }
    if let Some(u) = mode_until {
        check_time_value(u)?;
    }

    let mut p = vec!["journalctl".to_string()];
    if mode.follow() {
        p.push("-f".to_string()); // §10 唯一真理：Range 不带 -f
    }
    p.push(format!("-u {}", shell_quote(service)));
    if let Some(s) = since_str {
        let abs = parse_journal_since(s); // "30m" → RFC3339；绝对时间原样
        p.push(format!("--since {}", shell_quote(&abs)));
    }
    if let Some(u) = mode_until {
        let abs = parse_journal_since(u);
        p.push(format!("--until {}", shell_quote(&abs)));
    }
    Ok(p.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 相对时长换算为绝对时间() {
        let abs = parse_journal_since("30m");
        assert!(abs.contains("2026"), "RFC3339 含年份: {abs}");
        assert!(abs.contains('T') || abs.contains('+') || abs.contains('Z'), "{abs}");
    }

    #[test]
    fn 绝对时间原样返回() {
        assert_eq!(parse_journal_since("today"), "today");
        assert_eq!(
            parse_journal_since("2026-01-05T10:00:00+08:00"),
            "2026-01-05T10:00:00+08:00"
        );
    }
}
