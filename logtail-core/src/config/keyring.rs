//! 系统密钥环存取（§16.3，[v0.10.9]）：SSH 密码出明文 config.toml，落 OS 凭据库。
//!
//! 条目约定：服务名固定 `logtail`，账户名默认 = 服务器 `name`（`AuthCfg::Keyring`
//! 的 `account` 可覆盖）。平台实现由 keyring crate 特性选择：Windows 凭据管理器 /
//! macOS 钥匙串 / Linux Secret Service（无头机器无服务时运行期报错——此时用户回退
//! 明文 `password` 认证即可，两种方式长期并存）。
//!
//! 写入入口是 TUI 子命令 `logtail-tui keyring set <服务器名>`（交互隐藏输入）；
//! 本模块只做纯函数封装，UI 无关。单测走共享内存凭据库（见 test_store），
//! 不依赖真实 OS 服务，CI 无头环境可跑。

use crate::error::LogtailError;

/// 凭据库服务名（Windows 凭据管理器里显示的"Internet 或网络地址"）
pub const SERVICE: &str = "logtail";

fn entry(account: &str) -> Result<keyring::Entry, LogtailError> {
    // 测试态：共享内存凭据库模拟真实存储的持久化语义（跨 Entry 复用同一份数据），
    // 不触 OS 密钥环；正式态：按平台特性创建真实凭据库条目
    #[cfg(test)]
    return Ok(test_store::entry(account));
    #[cfg(not(test))]
    keyring::Entry::new(SERVICE, account)
        .map_err(|e| LogtailError::Keyring(format!("创建条目失败（{account}）: {e}")))
}

/// 取密码。NoEntry 给出可操作指引；其余 keyring 错误原样带出。
pub fn get(account: &str) -> Result<String, LogtailError> {
    match entry(account)?.get_password() {
        Ok(pw) => Ok(pw),
        Err(keyring::Error::NoEntry) => Err(LogtailError::Keyring(format!(
            "密钥环无 '{account}' 条目——先执行 `logtail-tui keyring set {account}` 写入，\
             或把认证改回 {{ type = \"password\" }}"
        ))),
        Err(e) => Err(LogtailError::Keyring(format!("读取失败（{account}）: {e}"))),
    }
}

/// 存密码（TUI `keyring set` 子命令调用；重复 set = 覆盖）
pub fn set(account: &str, password: &str) -> Result<(), LogtailError> {
    entry(account)?
        .set_password(password)
        .map_err(|e| LogtailError::Keyring(format!("写入失败（{account}）: {e}")))
}

/// 删条目（TUI `keyring del` 子命令调用）；条目不存在也视为成功（幂等）
pub fn delete(account: &str) -> Result<(), LogtailError> {
    match entry(account)?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(LogtailError::Keyring(format!("删除失败（{account}）: {e}"))),
    }
}

#[cfg(test)]
mod test_store {
    //! 共享内存凭据库（仅测试）：按账户共享同一份数据，模拟真实密钥库的
    //! "存进去下次取还在" 语义。keyring crate 自带的 mock 是 EntryOnly
    //! （密码存在 Entry 里，跨 `Entry::new` 不存活），无法测我们的 get/set/delete。
    use keyring::credential::CredentialApi;
    use keyring::Error as KError;
    use std::any::Any;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};

    #[derive(Debug, Default)]
    struct TestData {
        secret: Option<String>,
    }

    #[derive(Debug)]
    struct TestCred(Arc<Mutex<TestData>>);

    impl CredentialApi for TestCred {
        fn set_password(&self, password: &str) -> Result<(), KError> {
            self.0.lock().unwrap().secret = Some(password.to_string());
            Ok(())
        }
        fn get_password(&self) -> Result<String, KError> {
            self.0.lock().unwrap().secret.clone().ok_or(KError::NoEntry)
        }
        // 字节接口走密码转换（本工具只用字符串密码；空 secret 视作无条目）
        fn set_secret(&self, secret: &[u8]) -> Result<(), KError> {
            let s = std::str::from_utf8(secret)
                .map_err(|_| KError::BadEncoding(secret.to_vec()))?;
            self.set_password(s)
        }
        fn get_secret(&self) -> Result<Vec<u8>, KError> {
            self.get_password().map(|s| s.into_bytes())
        }
        fn delete_credential(&self) -> Result<(), KError> {
            let mut d = self.0.lock().unwrap();
            d.secret.take().ok_or(KError::NoEntry).map(|_| ())
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    /// 按 account 取（或建）共享数据并包成 Entry——同一账户的每次 entry() 指向同一份数据
    pub fn entry(account: &str) -> keyring::Entry {
        static STORE: OnceLock<Mutex<HashMap<String, Arc<Mutex<TestData>>>>> = OnceLock::new();
        let store = STORE.get_or_init(|| Mutex::new(HashMap::new()));
        let mut map = store.lock().unwrap();
        let data = map
            .entry(account.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(TestData::default())));
        keyring::Entry::new_with_credential(Box::new(TestCred(Arc::clone(data))))
    }
}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]

    use super::*;

    #[test]
    fn 密钥环_存取删往返() {
        set("acct-a", "pw123").unwrap();
        assert_eq!(get("acct-a").unwrap(), "pw123");
        set("acct-a", "pw456").unwrap(); // 覆盖写
        assert_eq!(get("acct-a").unwrap(), "pw456");
        delete("acct-a").unwrap();
        delete("acct-a").unwrap(); // 幂等
        let err = get("acct-a").unwrap_err().to_string();
        assert!(err.contains("keyring set acct-a"), "缺条目报错带指引: {err}");
    }
}
