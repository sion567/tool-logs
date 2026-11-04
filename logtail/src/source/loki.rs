//! Loki 数据源（§20 v0.8）：远端 curl + jq 查询 Loki HTTP API，stdout 按行输出
//! `RFC3339 原始行`——复用既有 SSH 命令流管线（核心不动），本地无需 HTTP 客户端。
//!
//! 远端依赖：curl + jq（USAGE 标注）。命令一次性返回后 Ended，回放更早用 L/f。

use chrono::DateTime;

use super::{check_time_value, shell_quote, ModeVariant};
use crate::error::LogtailError;

/// [v0.8] Loki URL 白名单：必须 http(s):// 开头，其余仅 URL 安全字符
pub fn check_loki_url(s: &str) -> Result<(), LogtailError> {
    let Some(rest) = s.strip_prefix("http://").or_else(|| s.strip_prefix("https://")) else {
        return Err(LogtailError::UnsafeInput(format!(
            "Loki URL 必须以 http:// 或 https:// 开头: {s}"
        )));
    };
    if rest.is_empty()
        || !rest
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '/' | '%' | '+'))
    {
        return Err(LogtailError::UnsafeInput(format!("Loki URL 非法: {s}")));
    }
    Ok(())
}

/// [v0.8] LogQL 查询白名单：比 grep pattern 宽（LogQL 需要双引号标签匹配），
/// 仍拒绝单引号/`$`/`;`/`&`/反引号/反引号——单引号包裹下这些是唯一的逃逸面。
pub fn check_logql(s: &str) -> Result<(), LogtailError> {
    if s.is_empty() {
        return Err(LogtailError::UnsafeInput("LogQL 查询为空".into()));
    }
    if s.chars().any(|c| {
        matches!(c, '\'' | '$' | ';' | '&' | '`') || (c as u32) < 0x20
    }) {
        return Err(LogtailError::UnsafeInput(format!("LogQL 查询非法: {s}")));
    }
    Ok(())
}

/// [v0.8] RFC3339 → Loki 需要的纳秒时间戳字符串
pub fn rfc3339_to_nanos(s: &str) -> Option<String> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .and_then(|t| t.timestamp_nanos_opt())
        .map(|n| n.to_string())
}

/// [v0.8] Quick 的 since 归一化：RFC3339 → None（调用方改用 start）；
/// 相对时长 h/m 透传（Loki 支持），d 不在 Go duration 里 → 换算成 h
pub fn loki_duration(s: &str) -> Option<String> {
    if rfc3339_to_nanos(s).is_some() {
        return None;
    }
    let re = regex::Regex::new(r"^(\d+)([hmd])$").unwrap();
    let caps = re.captures(s)?;
    let n: i64 = caps[1].parse().unwrap_or(1);
    Some(match &caps[2] {
        "d" => format!("{}h", n.saturating_mul(24)),
        other => format!("{n}{other}"),
    })
}

/// [v0.8] 生成 Loki 查询命令。三种模式：
/// - Full：最新 limit 条（backward 取回 + jq 全局按时间排序，缓冲区保持旧→新）
/// - Quick{since}：回放 since 以来（相对时长或绝对时间）
/// - Range(s,u)：start/end 纳秒精确窗口
///
/// jq `sort_by` 保证跨流全局时间序；strftime 前缀时间戳供 parse_line 提取。
pub fn loki_command(
    url: &str,
    query: &str,
    limit: usize,
    mode: &ModeVariant,
) -> Result<String, LogtailError> {
    check_loki_url(url)?;
    check_logql(query)?;
    let limit = limit.clamp(1, 5000);

    // [v0.8] name=value 形式（curl 支持），值统一走 shell_quote 单引号包裹——不再手动加引号
    let mut params = vec![
        format!("--data-urlencode query={}", shell_quote(query)),
        format!("--data-urlencode limit={limit}"),
    ];
    match mode {
        ModeVariant::Full => {} // 最新 limit 条
        ModeVariant::Quick { since } => {
            if let Some(s) = since.as_deref() {
                match loki_duration(s) {
                    Some(d) => params.push(format!("--data-urlencode since={d}")),
                    None => {
                        let ns = rfc3339_to_nanos(s)
                            .ok_or_else(|| LogtailError::ConfigError(format!("since 无法解析: {s}")))?;
                        params.push(format!("--data-urlencode start={ns}"));
                    }
                }
            }
        }
        ModeVariant::Range(s, u) => {
            check_time_value(s)?;
            check_time_value(u)?;
            let s_ns = rfc3339_to_nanos(s).ok_or_else(|| {
                LogtailError::ConfigError("精准模式时间必须是 RFC3339（Loki 源）".into())
            })?;
            let u_ns = rfc3339_to_nanos(u).ok_or_else(|| {
                LogtailError::ConfigError("精准模式时间必须是 RFC3339（Loki 源）".into())
            })?;
            params.push(format!("--data-urlencode start={s_ns}"));
            params.push(format!("--data-urlencode end={u_ns}"));
        }
    }

    let jq = r#"[.data.result[].values[]] | sort_by(.[0]) | .[] | (.[0] | tonumber / 1e9 | strftime("%Y-%m-%dT%H:%M:%SZ")) + " " + .[1]"#;
    Ok(format!(
        "curl -sG {} {} --data-urlencode direction=backward | jq -r {}",
        shell_quote(url),
        params.join(" "),
        shell_quote(jq)
    ))
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]

    use super::*;

    #[test]
    fn loki_url校验() {
        assert!(check_loki_url("http://localhost:3100").is_ok());
        assert!(check_loki_url("https://loki.internal:3100/").is_ok());
        assert!(check_loki_url("ftp://x").is_err());
        assert!(check_loki_url("http://a b").is_err(), "空格拒绝");
        assert!(check_loki_url("localhost:3100").is_err(), "必须带协议");
    }

    #[test]
    fn logql校验_放行双引号拒杀手字符() {
        assert!(check_logql(r#"{job="myapp"} |= "ERROR""#).is_ok());
        assert!(check_logql("{job=~\"api-.*\"}").is_ok());
        assert!(check_logql("{job=\"x\"}'; rm -rf /").is_err()); // 单引号
        assert!(check_logql("{job=\"x\"}$(id)").is_err()); // $
        assert!(check_logql("{job=\"x\"}&wc").is_err()); // &
        assert!(check_logql("{job=\"x\"}`id`").is_err()); // 反引号
        assert!(check_logql("").is_err());
    }

    #[test]
    fn ns转换与时长归一() {
        assert_eq!(
            rfc3339_to_nanos("1970-01-01T00:00:01Z").as_deref(),
            Some("1000000000")
        );
        assert_eq!(loki_duration("1h").as_deref(), Some("1h"));
        assert_eq!(loki_duration("2d").as_deref(), Some("48h"), "d 换算成 h");
        assert_eq!(loki_duration("30m").as_deref(), Some("30m"));
        // RFC3339 返回 None（调用方改用 start）
        assert!(loki_duration("2026-11-05T10:00:00+08:00").is_none());
    }

    #[test]
    fn loki命令_三模式() {
        let url = "http://localhost:3100";
        let q = r#"{job="myapp"}"#;
        // Full：最新 N 条，无时间参数
        let full = loki_command(url, q, 1000, &ModeVariant::Full).unwrap();
        assert!(full.starts_with("curl -sG http://localhost:3100"), "{full}");
        assert!(full.contains("query='{job=\"myapp\"}'"), "{full}");
        assert!(full.contains("limit=1000") && full.contains("sort_by"), "{full}");
        assert!(!full.contains("since=") && !full.contains("start="), "{full}");
        // Quick 相对时长
        let quick = loki_command(url, q, 500, &ModeVariant::Quick { since: Some("1h".into()) }).unwrap();
        assert!(quick.contains("since=1h"), "{quick}");
        // Quick 绝对时间 → start
        let quick_abs = loki_command(
            url,
            q,
            500,
            &ModeVariant::Quick { since: Some("1970-01-01T00:00:01Z".into()) },
        )
        .unwrap();
        assert!(quick_abs.contains("start=1000000000"), "{quick_abs}");
        // Range：start/end 纳秒
        let range = loki_command(
            url,
            q,
            500,
            &ModeVariant::Range("1970-01-01T00:00:01Z".into(), "1970-01-01T00:00:02Z".into()),
        )
        .unwrap();
        assert!(range.contains("start=1000000000") && range.contains("end=2000000000"), "{range}");
        // 注入面：query 含单引号被拒
        assert!(loki_command(url, "{job=\"x\"}'; rm", 100, &ModeVariant::Full).is_err());
    }
}
