//! 日志源抽象（§6）：LogSource / SourceType / ModeVariant / Preset / 安全校验

pub mod docker;
pub mod file;
pub mod journal;

use serde::Deserialize;

use crate::error::LogtailError;
use crate::log::LevelFilter;

/// 解析器用：是否拆 docker JSON 包壳（§6.1 [v0.4 P2#6]）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceType {
    Docker,
    File,
    Journalctl,
    Command,
}

/// 日志源声明（§6.1）。`tail` 缺省 100；`follow` 只是声明，由 ModeVariant 决策。
/// rename_all：config 里写小写 `type = "docker"|"file"|"journalctl"|"command"`。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum LogSource {
    Docker {
        container: String,
        #[serde(default = "d_tail")]
        tail: usize,
        #[serde(default)]
        follow: bool,
        #[serde(default)]
        since: Option<String>,
        #[serde(default)]
        until: Option<String>,
    },
    File {
        path: String,
        #[serde(default = "d_tail")]
        tail: usize,
    },
    Journalctl {
        service: String,
        /// 配置级默认；运行时优先 mode（§6.1）
        #[serde(default)]
        since: Option<String>,
    },
    /// 高级：用户自己保证安全（§16.2）
    Command { cmd: String },
}

fn d_tail() -> usize {
    100
}

impl LogSource {
    pub fn source_type(&self) -> SourceType {
        match self {
            LogSource::Docker { .. } => SourceType::Docker,
            LogSource::File { .. } => SourceType::File,
            LogSource::Journalctl { .. } => SourceType::Journalctl,
            LogSource::Command { .. } => SourceType::Command,
        }
    }

    /// 产出一个可在远端 shell 执行的命令串。
    /// 不变式：所有用户可控字段先 check_ident / check_time_value，再 shell_quote（§3.3③）。
    pub fn to_command(&self, mode: &ModeVariant) -> Result<String, LogtailError> {
        match self {
            LogSource::Docker { container, tail, follow, since, until } => {
                docker::docker_command(container, *tail, *follow, since.as_ref(), until.as_ref(), mode)
            }
            LogSource::File { path, tail } => file::file_command(path, *tail, mode),
            LogSource::Journalctl { service, since } => {
                journal::journal_command(service, since.as_ref(), mode)
            }
            LogSource::Command { cmd } => Ok(cmd.clone()), // 不走校验，用户自己承担
        }
    }
}

/// 模式系统（§10）：`-f`/`--until` 的唯一决策者；源结构体只存声明，不自作主张 follow。
#[derive(Debug, Clone, Default)]
pub enum ModeVariant {
    #[default]
    Full,
    Quick { since: Option<String> },
    /// (since, until)，不带 -f
    Range(String, String),
}

impl ModeVariant {
    pub fn follow(&self) -> bool {
        !matches!(self, ModeVariant::Range(_, _))
    }

    /// 注意：只有 docker 分支用它（.or() 兜底）；journal 分支直接 match mode，
    /// 不绕这里——Range 下它恒 None，v0.3 因此丢了时间窗 [v0.4 P0#3]。
    pub fn since(&self) -> Option<&str> {
        match self {
            ModeVariant::Quick { since: s } => s.as_deref(),
            _ => None,
        }
    }

    pub fn until(&self) -> Option<&str> {
        match self {
            ModeVariant::Range(_, u) => Some(u),
            _ => None,
        }
    }

    pub fn label(&self) -> String {
        match self {
            ModeVariant::Full => "全量".to_string(),
            ModeVariant::Quick { since: Some(s) } => format!("快捷({s})"),
            ModeVariant::Quick { since: None } => "快捷".to_string(),
            ModeVariant::Range(_, _) => "精准".to_string(),
        }
    }
}

/// 预设（§10 [v0.4 三轮]）：把 level + since 打包成一次按键，作用于当前激活源。
/// level 是本地过滤（立即生效、不重连）；since 会改命令，必须走重连——
/// 与 R 完全同一条路径。apply 只改状态，重连由调用方（main_loop 的 pending_action）执行。
#[derive(Debug, Clone)]
pub struct Preset {
    /// config 里的 [presets.<key>] 键
    pub key: String,
    pub name: String,
    pub level: Option<LevelFilter>,
    pub since: Option<String>,
}

impl Preset {
    pub fn apply(&self, st: &mut crate::app::AppState) {
        if let Some(lf) = self.level {
            if st.level_filter != lf {
                st.level_filter = lf;
                st.last_rebuild += 1;
            }
        }
        if let Some(s) = self.since.clone() {
            st.mode = ModeVariant::Quick { since: Some(s) };
        }
    }
}

// ---- 安全校验（§6.3）----

/// 白名单：字母数字 + 容器名/路径/服务名常见符号。
/// 空格、引号、分号、管道、重定向、反引号 → 一律拒绝。
pub fn is_safe_ident(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    s.chars().all(|c| {
        c.is_alphanumeric() || matches!(c, '-' | '_' | '/' | '.' | ':' | '@')
    })
}

pub fn check_ident(s: &str, what: &str) -> Result<(), LogtailError> {
    if is_safe_ident(s) {
        Ok(())
    } else {
        Err(LogtailError::UnsafeInput(format!("{what}非法: {s}")))
    }
}

/// 仅对"已通过白名单"的字符串做兜底引用；未通过的绝不拼进命令。
pub fn shell_quote(s: &str) -> String {
    if is_safe_ident(s) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// [v0.4 P2#4] 时间值白名单（since/until 专用）：比容器名宽——绝对时间戳里的
/// `- : . +` 与空格都必须放行（`2026-01-05T10:00:00+08:00`、`2026-01-05 14:02:04`），
/// 否则被 is_safe_ident 误杀；但 `;` 引号管道反引号仍一律拒绝。
pub fn is_safe_time(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    s.chars().all(|c| {
        c.is_alphanumeric() || matches!(c, '-' | ':' | '.' | '+' | ' ')
    })
}

pub fn check_time_value(s: &str) -> Result<(), LogtailError> {
    if is_safe_time(s) {
        Ok(())
    } else {
        Err(LogtailError::UnsafeInput(format!("时间值非法: {s}")))
    }
}

/// [v0.6 §16.6] grep pattern 白名单：正则元字符（|()[]{}.*+?^$\ 等）要放行否则 ERE 没法用，
/// 但 shell 杀手字符仍拒绝——单引号/双引号/反引号/$/;/&，以及控制字符。
/// （$ 在单引号内本无害，但显式拒绝可把注入面压到最小，符合不变式③"先校验再 quote"。）
pub fn is_safe_pattern(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    s.chars().all(|c| {
        c.is_alphanumeric()
            || matches!(
                c,
                ' ' | '-' | '_' | '.' | ':' | '/' | ',' | '?' | '=' | '+' | '%' | '@' | '#'
                    | '~' | '^' | '*' | '[' | ']' | '(' | ')' | '{' | '}' | '|' | '<' | '>'
                    | '!' | '\\'
            )
    })
}

pub fn check_pattern(s: &str) -> Result<(), LogtailError> {
    if is_safe_pattern(s) {
        Ok(())
    } else {
        Err(LogtailError::UnsafeInput(format!("过滤 pattern 非法: {s}")))
    }
}

/// [v0.6] 把远端预过滤接到命令尾部：`| grep -E --line-buffered '<pattern>'`。
/// --line-buffered 防 grep 块缓冲延迟出行；-E 用 ERE（与本地 Regex 模式同语义族）。
/// Command 源的用户自担命令同样适用（管道对任意命令成立）。
pub fn apply_prefilter(cmd: &str, pattern: &str) -> Result<String, LogtailError> {
    check_pattern(pattern)?;
    Ok(format!(
        "{cmd} | grep -E --line-buffered {}",
        shell_quote(pattern)
    ))
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]

    use super::*;

    #[test]
    fn 非法输入_被拒绝_而非转义放行() {
        let src = LogSource::Docker {
            container: "app;rm -rf /".into(),
            tail: 100,
            follow: true,
            since: None,
            until: None,
        };
        let err = src.to_command(&ModeVariant::Full).unwrap_err();
        assert!(matches!(err, LogtailError::UnsafeInput(_)));
    }

    #[test]
    fn journalctl_range_不带_f() {
        let src = LogSource::Journalctl { service: "nginx".into(), since: None };
        let cmd = src
            .to_command(&ModeVariant::Range(
                "2026-01-05T10:00:00+08:00".into(),
                "2026-01-05T11:00:00+08:00".into(),
            ))
            .unwrap();
        assert!(!cmd.contains(" -f"), "Range 禁止 -f: {cmd}");
        assert!(cmd.contains("--since") && cmd.contains("--until"), "{cmd}");
    }

    #[test]
    fn journalctl_quick_since_取自_mode() {
        let src = LogSource::Journalctl { service: "nginx".into(), since: None };
        let cmd = src
            .to_command(&ModeVariant::Quick { since: Some("30m".into()) })
            .unwrap();
        assert!(cmd.contains("--since") && cmd.contains("2026"), "{cmd}");
    }

    #[test]
    fn since值_先校验再拼接() {
        assert!(check_time_value("2026-01-05T10:00:00+08:00").is_ok());
        assert!(check_time_value("2026-01-05 14:02:04").is_ok());
        assert!(check_time_value("30m").is_ok());
        assert!(matches!(check_time_value("x;rm"), Err(LogtailError::UnsafeInput(_))));
    }

    #[test]
    fn pattern_白名单放行正则拒绝杀手字符() {
        assert!(check_pattern("db error|timeout.*retry").is_ok());
        assert!(check_pattern("ERR(?i)x").is_ok()); // ? ( ) i 放行
        assert!(check_pattern("^\\d{3}-[A-Z]+").is_ok()); // ^ { } [ ] - 反斜杠 放行
        assert!(matches!(check_pattern("x'; rm -rf /"), Err(LogtailError::UnsafeInput(_))));
        assert!(matches!(check_pattern("a`id`b"), Err(LogtailError::UnsafeInput(_))));
        assert!(matches!(check_pattern("$(id)"), Err(LogtailError::UnsafeInput(_)))); // $ 拒
        assert!(matches!(check_pattern("a&b"), Err(LogtailError::UnsafeInput(_))));
        assert!(matches!(check_pattern(""), Err(LogtailError::UnsafeInput(_))));
    }

    #[test]
    fn apply_prefilter_管道引用与拒绝() {
        let cmd = apply_prefilter("docker logs -f x", "db error").unwrap();
        assert_eq!(cmd, "docker logs -f x | grep -E --line-buffered 'db error'");
        // 含引号的 pattern 被白名单拒绝而非转义放行（不变式③）
        assert!(apply_prefilter("cmd", "a'b").is_err());
        assert!(apply_prefilter("cmd", "a;b").is_err());
    }

    #[test]
    fn docker_range_无f且带until() {
        let src = LogSource::Docker {
            container: "my-app".into(),
            tail: 200,
            follow: true,
            since: None,
            until: None,
        };
        let cmd = src
            .to_command(&ModeVariant::Range("2026-01-05 10:00:00".into(), "2026-01-05 11:00:00".into()))
            .unwrap();
        assert!(!cmd.contains(" -f"), "{cmd}");
        assert!(cmd.contains("--since") && cmd.contains("--until"), "{cmd}");
        assert!(cmd.contains("my-app"), "{cmd}");
    }
}
