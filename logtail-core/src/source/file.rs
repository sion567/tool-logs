//! tail 参数构造（§6.2 file 分支）

use super::{check_ident, shell_quote, ModeVariant};
use crate::error::LogtailError;

pub(super) fn file_command(
    path: &str,
    tail: usize,
    mode: &ModeVariant,
) -> Result<String, LogtailError> {
    check_ident(path, "文件路径")?;
    Ok(match mode {
        ModeVariant::Range(_, _) => format!("tail -n {tail} {}", shell_quote(path)),
        // -F 跟 inode 抗轮转
        _ => format!("tail -F -n {tail} {}", shell_quote(path)),
    })
}
