//! config.toml 的 GUI 管理（[v0.2.4]）：服务器/源/预设的增删改 + remember_last_source 开关。
//!
//! 分两层：
//! - 纯函数层（本文件上半部）：文本 → toml_edit 解析 → 定点修改 → 文本。**保留用户
//!   注释与排版**（只替换被改的值，decor 随迁）；单测覆盖增删改往返。
//! - 窗口层（`ManageState` + `impl LogTailApp`）：三节列表 + 内嵌表单，保存走纯函数层，
//!   写盘前自动备份 `config.toml.bak`，写后重载 `remote_cfg`。
//!
//! 认证支持三种：密码明文 / 密钥文件 / 密钥环（密码写系统凭据库，config 只留
//! `auth = { type = "keyring" }`，与 TUI `logtail-tui keyring set` 同库）。

use eframe::egui::{self, Context, RichText};
use logtail_core::config::Config;
use toml_edit::{value, DocumentMut, InlineTable, Item, Table, Value};

use crate::ui::{LogTailApp, ERR_FG};

// ---------- 表单草稿（GUI 侧数据，与 toml 字段一一对应） ----------

#[derive(Debug, Clone, PartialEq)]
pub enum AuthDraft {
    /// 明文密码（写入 config.toml）
    Password(String),
    /// 密钥文件（path + 可选口令）
    Key {
        path: String,
        passphrase: Option<String>,
    },
    /// 密钥环：config 只写 type="keyring"，密码在系统凭据库（账户名 = 服务器名）
    Keyring,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ServerDraft {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: AuthDraft,
    pub sudo: bool,
    /// None/空 = 未设（sudo 密码缺省复用登录密码）
    pub sudo_password: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SourceKindDraft {
    Docker {
        container: String,
        tail: usize,
        follow: bool,
        since: Option<String>,
    },
    File {
        path: String,
        tail: usize,
    },
    Journalctl {
        service: String,
        since: Option<String>,
    },
    Command {
        cmd: String,
    },
    Loki {
        url: String,
        query: String,
        limit: usize,
    },
    LocalFile {
        path: String,
        tail: usize,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct SourceDraft {
    pub name: String,
    /// 所属服务器名；local 型可空（省略）
    pub server: String,
    pub kind: SourceKindDraft,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PresetDraft {
    pub key: String,
    pub name: Option<String>,
    pub level: Option<String>,
    pub since: Option<String>,
}

// ---------- 纯函数层：text → text ----------

fn parse_doc(text: &str) -> Result<DocumentMut, String> {
    text.parse::<DocumentMut>()
        .map_err(|e| format!("config.toml 解析失败: {e}"))
}

/// 定点替换表内键值，保留原值的 decor（行首缩进与行尾内联注释）；
/// 新键正常插入。空字符串值的 Optional 键交给调用方 remove。
fn set_item(tbl: &mut Table, key: &str, mut new: Item) {
    if let Some(Item::Value(old)) = tbl.get(key)
        && let Item::Value(v) = &mut new
    {
        let decor = old.decor().clone();
        *v.decor_mut() = decor;
    }
    tbl.insert(key, new);
}

fn set_str(tbl: &mut Table, key: &str, s: &str) {
    set_item(tbl, key, value(s));
}

fn ensure_aot<'a>(
    doc: &'a mut DocumentMut,
    key: &'static str,
) -> Result<&'a mut toml_edit::ArrayOfTables, String> {
    if doc.get(key).is_none() {
        doc.insert(key, Item::ArrayOfTables(toml_edit::ArrayOfTables::new()));
    }
    doc[key]
        .as_array_of_tables_mut()
        .ok_or(format!("[{key}] 不是 [[数组表]]，请手工检查 config.toml"))
}

/// remember_last_source 开关（设置菜单）：[general] 缺失时自动创建
pub fn set_remember_last_source(text: &str, on: bool) -> Result<String, String> {
    let mut doc = parse_doc(text)?;
    if doc.get("general").is_none() {
        doc.insert("general", Item::Table(Table::new()));
    }
    let g = doc["general"]
        .as_table_mut()
        .ok_or("[general] 不是表，请手工检查 config.toml")?;
    set_item(g, "remember_last_source", value(on));
    Ok(doc.to_string())
}

fn auth_item(a: &AuthDraft) -> Item {
    let mut it = InlineTable::new();
    match a {
        AuthDraft::Password(p) => {
            it.insert("type", Value::from("password"));
            it.insert("password", Value::from(p.clone()));
        }
        AuthDraft::Key { path, passphrase } => {
            it.insert("type", Value::from("key"));
            it.insert("path", Value::from(path.clone()));
            if let Some(pp) = passphrase.as_deref().filter(|s| !s.is_empty()) {
                it.insert("passphrase", Value::from(pp));
            }
        }
        AuthDraft::Keyring => {
            it.insert("type", Value::from("keyring"));
        }
    }
    Item::Value(it.into())
}

/// 新增或原位更新 [[servers]] 条目（按 name 匹配；保留表内其余未管键与注释）
pub fn upsert_server(text: &str, d: &ServerDraft) -> Result<String, String> {
    let mut doc = parse_doc(text)?;
    let aot = ensure_aot(&mut doc, "servers")?;
    let existing = aot
        .iter_mut()
        .find(|t| t.get("name").and_then(|v| v.as_str()) == Some(d.name.as_str()));
    match existing {
        Some(t) => {
            let tbl = t; // ArrayOfTables::iter_mut 直接给 &mut Table
            set_str(tbl, "host", &d.host);
            set_item(tbl, "port", value(d.port as i64));
            set_str(tbl, "user", &d.user);
            set_item(tbl, "auth", auth_item(&d.auth));
            set_item(tbl, "sudo", value(d.sudo));
            match d.sudo_password.as_deref().filter(|s| !s.is_empty()) {
                Some(sp) => set_str(tbl, "sudo_password", sp),
                None => {
                    tbl.remove("sudo_password");
                }
            }
        }
        None => {
            let mut t = Table::new();
            t.insert("name", value(d.name.clone()));
            t.insert("host", value(d.host.clone()));
            t.insert("port", value(d.port as i64));
            t.insert("user", value(d.user.clone()));
            t.insert("auth", auth_item(&d.auth));
            t.insert("sudo", value(d.sudo));
            if let Some(sp) = d.sudo_password.as_deref().filter(|s| !s.is_empty()) {
                t.insert("sudo_password", value(sp));
            }
            aot.push(t);
        }
    }
    Ok(doc.to_string())
}

pub fn remove_server(text: &str, name: &str) -> Result<String, String> {
    let mut doc = parse_doc(text)?;
    let aot = ensure_aot(&mut doc, "servers")?;
    let idx = aot
        .iter()
        .position(|t| t.get("name").and_then(|v| v.as_str()) == Some(name))
        .ok_or(format!("服务器 {name} 不存在"))?;
    aot.remove(idx);
    Ok(doc.to_string())
}

/// 源类型 → 内联表（type 键在最前，字段序与模板一致）
fn source_inline(kind: &SourceKindDraft) -> InlineTable {
    let mut it = InlineTable::new();
    match kind {
        SourceKindDraft::Docker {
            container,
            tail,
            follow,
            since,
        } => {
            it.insert("type", Value::from("docker"));
            it.insert("container", Value::from(container.clone()));
            it.insert("tail", Value::from(*tail as i64));
            it.insert("follow", Value::from(*follow));
            if let Some(s) = since.as_deref().filter(|s| !s.is_empty()) {
                it.insert("since", Value::from(s));
            }
        }
        SourceKindDraft::File { path, tail } => {
            it.insert("type", Value::from("file"));
            it.insert("path", Value::from(path.clone()));
            it.insert("tail", Value::from(*tail as i64));
        }
        SourceKindDraft::Journalctl { service, since } => {
            it.insert("type", Value::from("journalctl"));
            it.insert("service", Value::from(service.clone()));
            if let Some(s) = since.as_deref().filter(|s| !s.is_empty()) {
                it.insert("since", Value::from(s));
            }
        }
        SourceKindDraft::Command { cmd } => {
            it.insert("type", Value::from("command"));
            it.insert("cmd", Value::from(cmd.clone()));
        }
        SourceKindDraft::Loki { url, query, limit } => {
            it.insert("type", Value::from("loki"));
            it.insert("url", Value::from(url.clone()));
            it.insert("query", Value::from(query.clone()));
            it.insert("limit", Value::from(*limit as i64));
        }
        SourceKindDraft::LocalFile { path, tail } => {
            it.insert("type", Value::from("local"));
            it.insert("path", Value::from(path.clone()));
            it.insert("tail", Value::from(*tail as i64));
        }
    }
    it
}

/// 新增或原位更新 [[sources]] 条目（按 name 匹配）
pub fn upsert_source(text: &str, d: &SourceDraft) -> Result<String, String> {
    let mut doc = parse_doc(text)?;
    let aot = ensure_aot(&mut doc, "sources")?;
    let inline = source_inline(&d.kind);
    let server_val = (!d.server.is_empty()).then(|| value(d.server.clone()));
    let existing = aot
        .iter_mut()
        .find(|t| t.get("name").and_then(|v| v.as_str()) == Some(d.name.as_str()));
    match existing {
        Some(t) => {
            let tbl = t;
            set_item(tbl, "source", Item::Value(inline.into()));
            match server_val {
                Some(v) => set_item(tbl, "server", v),
                None => {
                    tbl.remove("server");
                }
            }
        }
        None => {
            let mut t = Table::new();
            t.insert("name", value(d.name.clone()));
            if let Some(v) = server_val {
                t.insert("server", v);
            }
            t.insert("source", Item::Value(inline.into()));
            aot.push(t);
        }
    }
    Ok(doc.to_string())
}

pub fn remove_source(text: &str, name: &str) -> Result<String, String> {
    let mut doc = parse_doc(text)?;
    let aot = ensure_aot(&mut doc, "sources")?;
    let idx = aot
        .iter()
        .position(|t| t.get("name").and_then(|v| v.as_str()) == Some(name))
        .ok_or(format!("源 {name} 不存在"))?;
    aot.remove(idx);
    Ok(doc.to_string())
}

/// 新增或原位更新 [presets.<key>]（name/level/since 全可省；空值键不写入）
pub fn upsert_preset(text: &str, d: &PresetDraft) -> Result<String, String> {
    let mut doc = parse_doc(text)?;
    if doc.get("presets").is_none() {
        doc.insert("presets", Item::Table(Table::new()));
    }
    let p = doc["presets"]
        .as_table_mut()
        .ok_or("[presets] 不是表，请手工检查 config.toml")?;
    let mut t = Table::new();
    for (key, v) in [
        ("name", d.name.as_deref()),
        ("level", d.level.as_deref()),
        ("since", d.since.as_deref()),
    ] {
        if let Some(s) = v.filter(|s| !s.is_empty()) {
            t.insert(key, value(s));
        }
    }
    p.insert(&d.key, Item::Table(t));
    Ok(doc.to_string())
}

pub fn remove_preset(text: &str, key: &str) -> Result<String, String> {
    let mut doc = parse_doc(text)?;
    let p = doc["presets"]
        .as_table_mut()
        .ok_or("[presets] 不是表，请手工检查 config.toml")?;
    if p.remove(key).is_none() {
        return Err(format!("预设 {key} 不存在"));
    }
    Ok(doc.to_string())
}

// ---------- 窗口层：管理状态 ----------

const AUTH_KINDS: [&str; 3] = ["密码（明文存 config）", "密钥文件", "密钥环（推荐）"];
const SOURCE_KINDS: [(&str, &str); 6] = [
    ("docker", "Docker 容器"),
    ("file", "远程文件"),
    ("journalctl", "journalctl 服务"),
    ("command", "远程命令"),
    ("loki", "Loki 查询"),
    ("local", "本地文件"),
];
const LEVEL_OPTIONS: [&str; 5] = ["（不设）", "error", "warn", "info", "debug"];

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Edit {
    None,
    /// Some(idx) = 编辑现有；None = 新增（idx 指向 remote_cfg.servers/sources 下标）
    Server(Option<usize>),
    Source(Option<usize>),
    /// Some(key) = 编辑现有预设；None = 新增
    Preset(Option<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DelTarget {
    Server(String),
    Source(String),
    Preset(String),
}

/// 服务器表单（String 承载输入，保存时校验转换）
#[derive(Debug, Clone, Default)]
struct ServerForm {
    name: String,
    host: String,
    port: u16,
    user: String,
    auth_kind: usize,
    password: String,
    key_path: String,
    passphrase: String,
    sudo: bool,
    sudo_password: String,
}

/// 源表单（六类源字段合一，按 kind_idx 取用）
#[derive(Debug, Clone, Default)]
struct SourceForm {
    name: String,
    server: String,
    kind_idx: usize,
    container: String,
    tail: usize,
    follow: bool,
    since: String,
    path: String,
    service: String,
    cmd: String,
    url: String,
    query: String,
    limit: usize,
}

#[derive(Debug, Clone, Default)]
struct PresetForm {
    key: String,
    name: String,
    level_idx: usize,
    since: String,
}

#[derive(Debug)]
pub(crate) struct ManageState {
    pub open: bool,
    edit: Edit,
    srv: ServerForm,
    src: SourceForm,
    pst: PresetForm,
    /// 删除二段确认（第一次点「删除」→ 变「确认删除？」）
    confirm_del: Option<DelTarget>,
    /// 表单校验/保存错误（红字显示在表单底部）
    form_err: Option<String>,
}

impl Default for ManageState {
    fn default() -> Self {
        Self {
            open: false,
            edit: Edit::None,
            srv: ServerForm {
                port: 22,
                ..Default::default()
            },
            src: SourceForm {
                tail: 100,
                follow: true,
                limit: 1000,
                ..Default::default()
            },
            pst: PresetForm::default(),
            confirm_del: None,
            form_err: None,
        }
    }
}

impl ManageState {
    /// 收起表单回到列表（窗口保持打开，删除确认一并复位）
    fn close_edit(&mut self) {
        self.edit = Edit::None;
        self.form_err = None;
        self.confirm_del = None;
    }
}

impl LogTailApp {
    /// [v0.2.4] 管理窗口（设置菜单 / 连接源对话框入口）
    pub(crate) fn manage_window(&mut self, ctx: &Context) {
        if !self.manage.open {
            return;
        }
        let mut open = self.manage.open;
        egui::Window::new("管理服务器与源（config.toml）")
            .open(&mut open)
            .default_width(660.0)
            .show(ctx, |ui| {
                if let Some(err) = &self.remote_cfg_err {
                    ui.colored_label(ERR_FG, format!("config.toml 加载失败: {err}"));
                    return;
                }
                let Some(cfg) = self.remote_cfg.clone() else {
                    return;
                };
                // 编辑表单区（若有）
                self.manage_form(ui, &cfg);
                ui.separator();
                // 三节列表
                self.manage_server_list(ui, &cfg);
                ui.separator();
                self.manage_source_list(ui, &cfg);
                ui.separator();
                self.manage_preset_list(ui, &cfg);
            });
        self.manage.open = open;
    }

    fn manage_form(&mut self, ui: &mut egui::Ui, cfg: &Config) {
        match self.manage.edit.clone() {
            Edit::None => {}
            Edit::Server(idx) => self.server_form(ui, cfg, idx),
            Edit::Source(idx) => self.source_form(ui, cfg, idx),
            Edit::Preset(key) => self.preset_form(ui, key),
        }
    }

    // ---- 服务器 ----

    fn manage_server_list(&mut self, ui: &mut egui::Ui, cfg: &Config) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("服务器").strong());
            if self.manage.edit == Edit::None
                && ui.small_button("＋ 新增").clicked()
            {
                self.manage.srv = ServerForm {
                    port: 22,
                    ..Default::default()
                };
                self.manage.edit = Edit::Server(None);
                self.manage.form_err = None;
            }
        });
        for (i, s) in cfg.servers.iter().enumerate() {
            let auth = s.auth.label();
            let sudo = if s.sudo { " · sudo" } else { "" };
            ui.horizontal(|ui| {
                ui.label(format!("{} → {}@{}:{}{} · {}", s.name, s.user, s.host, s.port, sudo, auth));
                if self.manage.edit == Edit::None && ui.small_button("编辑").clicked() {
                    self.start_edit_server(i);
                }
                self.delete_button(
                    ui,
                    DelTarget::Server(s.name.clone()),
                    format!("删除服务器 {}", s.name),
                );
            });
        }
        if cfg.servers.is_empty() {
            ui.label(RichText::new("（还没有服务器——新增后才能建远程源）").weak());
        }
    }

    fn start_edit_server(&mut self, idx: usize) {
        let Some(s) = self.remote_cfg.as_ref().and_then(|c| c.servers.get(idx)) else {
            return;
        };
        let (auth_kind, password, key_path, passphrase) = match &s.auth {
            logtail_core::config::AuthCfg::Password { password } => {
                (0, password.clone(), String::new(), String::new())
            }
            logtail_core::config::AuthCfg::Key { path, passphrase } => (
                1,
                String::new(),
                path.clone(),
                passphrase.clone().unwrap_or_default(),
            ),
            logtail_core::config::AuthCfg::Keyring { .. } => (2, String::new(), String::new(), String::new()),
        };
        self.manage.srv = ServerForm {
            name: s.name.clone(),
            host: s.host.clone(),
            port: s.port,
            user: s.user.clone(),
            auth_kind,
            password,
            key_path,
            passphrase,
            sudo: s.sudo,
            sudo_password: s.sudo_password.clone().unwrap_or_default(),
        };
        self.manage.edit = Edit::Server(Some(idx));
        self.manage.form_err = None;
    }

    fn server_form(&mut self, ui: &mut egui::Ui, cfg: &Config, idx: Option<usize>) {
        ui.label(RichText::new(if idx.is_some() { "编辑服务器" } else { "新增服务器" }).strong());
        let f = &mut self.manage.srv;
        egui::Grid::new("srv_form").num_columns(2).show(ui, |ui| {
            ui.label("名称");
            ui.add(egui::TextEdit::singleline(&mut f.name).desired_width(220.0));
            ui.end_row();
            ui.label("主机");
            ui.add(egui::TextEdit::singleline(&mut f.host).desired_width(220.0));
            ui.end_row();
            ui.label("端口");
            ui.add(egui::DragValue::new(&mut f.port).range(1..=65535));
            ui.end_row();
            ui.label("用户");
            ui.add(egui::TextEdit::singleline(&mut f.user).desired_width(220.0));
            ui.end_row();
            ui.label("认证");
            egui::ComboBox::from_id_salt("srv_auth")
                .selected_text(AUTH_KINDS[f.auth_kind])
                .show_ui(ui, |ui| {
                    for (i, label) in AUTH_KINDS.iter().enumerate() {
                        ui.selectable_value(&mut f.auth_kind, i, *label);
                    }
                });
            ui.end_row();
            match f.auth_kind {
                0 => {
                    ui.label("密码");
                    ui.add(egui::TextEdit::singleline(&mut f.password).password(true).desired_width(220.0));
                    ui.end_row();
                }
                1 => {
                    ui.label("密钥路径");
                    ui.add(egui::TextEdit::singleline(&mut f.key_path).desired_width(220.0));
                    ui.end_row();
                    ui.label("口令（可选）");
                    ui.add(egui::TextEdit::singleline(&mut f.passphrase).password(true).desired_width(220.0));
                    ui.end_row();
                }
                _ => {
                    ui.label("密码");
                    ui.add(egui::TextEdit::singleline(&mut f.password).password(true).desired_width(220.0))
                        .on_hover_text("写入系统密钥环（账户 = 服务器名）；留空 = 沿用已有条目");
                    ui.end_row();
                }
            }
            ui.label("sudo 前缀");
            ui.checkbox(&mut f.sudo, "远端命令加 sudo（docker 归 root 的机器）");
            ui.end_row();
            if f.sudo {
                ui.label("sudo 密码");
                ui.add(egui::TextEdit::singleline(&mut f.sudo_password).password(true).desired_width(220.0))
                    .on_hover_text("留空 = 复用登录密码");
                ui.end_row();
            }
        });
        if f.auth_kind == 2 {
            ui.label(
                RichText::new("密钥环认证：密码存系统凭据库，config.toml 不落明文（可先用 TUI `logtail-tui keyring set <名>` 写入）")
                    .weak(),
            );
        }
        self.form_actions(ui, |app| app.save_server_form(cfg));
    }

    /// [v0.2.4] 保存服务器表单：校验 → 密钥环可选写入 → upsert → 落盘重载
    fn save_server_form(&mut self, cfg: &Config) -> Result<(), String> {
        let f = self.manage.srv.clone();
        let name = f.name.trim().to_string();
        if name.is_empty() {
            return Err("名称不能为空".into());
        }
        // 编辑模式下允许保持原名（同名 = 自己）；新增/改名撞已有名 → 拒绝
        let editing_same = match &self.manage.edit {
            Edit::Server(Some(idx)) => self
                .remote_cfg
                .as_ref()
                .and_then(|c| c.servers.get(*idx))
                .is_some_and(|s| s.name == name),
            _ => false,
        };
        if !editing_same && cfg.servers.iter().any(|s| s.name == name) {
            return Err(format!("服务器 {name} 已存在（名称是唯一键）"));
        }
        if f.host.trim().is_empty() || f.user.trim().is_empty() {
            return Err("主机与用户不能为空".into());
        }
        let auth = match f.auth_kind {
            0 => {
                if f.password.is_empty() {
                    return Err("密码认证需要密码".into());
                }
                AuthDraft::Password(f.password.clone())
            }
            1 => {
                if f.key_path.trim().is_empty() {
                    return Err("密钥认证需要密钥路径".into());
                }
                AuthDraft::Key {
                    path: f.key_path.trim().to_string(),
                    passphrase: (!f.passphrase.is_empty()).then(|| f.passphrase.clone()),
                }
            }
            _ => {
                // 密钥环：填了新密码就写入系统凭据库（账户 = 服务器名）
                if !f.password.is_empty()
                    && let Err(e) = logtail_core::config::keyring::set(&name, &f.password)
                {
                    return Err(format!("密钥环写入失败: {e}"));
                }
                AuthDraft::Keyring
            }
        };
        let draft = ServerDraft {
            name,
            host: f.host.trim().to_string(),
            port: f.port,
            user: f.user.trim().to_string(),
            auth,
            sudo: f.sudo,
            sudo_password: (!f.sudo_password.is_empty()).then(|| f.sudo_password.clone()),
        };
        self.write_config_transformed(|text| upsert_server(text, &draft), "服务器已保存")
    }

    // ---- 源 ----

    fn manage_source_list(&mut self, ui: &mut egui::Ui, cfg: &Config) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("日志源").strong());
            if self.manage.edit == Edit::None && ui.small_button("＋ 新增").clicked() {
                self.manage.src = SourceForm {
                    tail: 100,
                    follow: true,
                    limit: 1000,
                    ..Default::default()
                };
                self.manage.edit = Edit::Source(None);
                self.manage.form_err = None;
            }
        });
        for (i, sc) in cfg.sources.iter().enumerate() {
            let ty = source_type_label(&sc.source);
            ui.horizontal(|ui| {
                let srv = if sc.server.is_empty() {
                    String::new()
                } else {
                    format!(" @{}", sc.server)
                };
                ui.label(format!("{}{srv} [{ty}]", sc.name));
                if self.manage.edit == Edit::None && ui.small_button("编辑").clicked() {
                    self.start_edit_source(i);
                }
                self.delete_button(
                    ui,
                    DelTarget::Source(sc.name.clone()),
                    format!("删除源 {}", sc.name),
                );
            });
        }
        if cfg.sources.is_empty() {
            ui.label(RichText::new("（还没有源——连接源对话框按这里列出的源连接）").weak());
        }
    }

    fn start_edit_source(&mut self, idx: usize) {
        use logtail_core::source::LogSource;
        let Some(sc) = self.remote_cfg.as_ref().and_then(|c| c.sources.get(idx)) else {
            return;
        };
        let mut f = SourceForm {
            name: sc.name.clone(),
            server: sc.server.clone(),
            ..Default::default()
        };
        match &sc.source {
            LogSource::Docker {
                container,
                tail,
                follow,
                since,
                ..
            } => {
                f.kind_idx = 0;
                f.container = container.clone();
                f.tail = *tail;
                f.follow = *follow;
                f.since = since.clone().unwrap_or_default();
            }
            LogSource::File { path, tail } => {
                f.kind_idx = 1;
                f.path = path.clone();
                f.tail = *tail;
            }
            LogSource::Journalctl { service, since } => {
                f.kind_idx = 2;
                f.service = service.clone();
                f.since = since.clone().unwrap_or_default();
            }
            LogSource::Command { cmd } => {
                f.kind_idx = 3;
                f.cmd = cmd.clone();
            }
            LogSource::Loki { url, query, limit } => {
                f.kind_idx = 4;
                f.url = url.clone();
                f.query = query.clone();
                f.limit = *limit;
            }
            LogSource::LocalFile { path, tail } => {
                f.kind_idx = 5;
                f.path = path.clone();
                f.tail = *tail;
            }
        }
        self.manage.src = f;
        self.manage.edit = Edit::Source(Some(idx));
        self.manage.form_err = None;
    }

    fn source_form(&mut self, ui: &mut egui::Ui, cfg: &Config, idx: Option<usize>) {
        ui.label(RichText::new(if idx.is_some() { "编辑源" } else { "新增源" }).strong());
        let f = &mut self.manage.src;
        let local = f.kind_idx == 5;
        egui::Grid::new("src_form").num_columns(2).show(ui, |ui| {
            ui.label("名称");
            ui.add(egui::TextEdit::singleline(&mut f.name).desired_width(220.0));
            ui.end_row();
            if !local {
                ui.label("服务器");
                egui::ComboBox::from_id_salt("src_server")
                    .selected_text(if f.server.is_empty() { "（选择）" } else { &f.server })
                    .show_ui(ui, |ui| {
                        for s in &cfg.servers {
                            ui.selectable_value(&mut f.server, s.name.clone(), &s.name);
                        }
                    });
                ui.end_row();
            }
            ui.label("类型");
            egui::ComboBox::from_id_salt("src_kind")
                .selected_text(SOURCE_KINDS[f.kind_idx].1)
                .show_ui(ui, |ui| {
                    for (i, (_, label)) in SOURCE_KINDS.iter().enumerate() {
                        ui.selectable_value(&mut f.kind_idx, i, *label);
                    }
                });
            ui.end_row();
            match f.kind_idx {
                0 => {
                    ui.label("容器名");
                    ui.add(egui::TextEdit::singleline(&mut f.container).desired_width(220.0));
                    ui.end_row();
                    ui.label("tail 行数");
                    ui.add(egui::DragValue::new(&mut f.tail).range(1..=100_000));
                    ui.end_row();
                    ui.label("跟随");
                    ui.checkbox(&mut f.follow, "docker -f（实时流）");
                    ui.end_row();
                    ui.label("since（可选）");
                    ui.add(egui::TextEdit::singleline(&mut f.since).desired_width(220.0))
                        .on_hover_text("docker 时间窗，如 1h / today；留空不设");
                    ui.end_row();
                }
                1 | 5 => {
                    ui.label("文件路径");
                    ui.add(egui::TextEdit::singleline(&mut f.path).desired_width(220.0));
                    ui.end_row();
                    ui.label("tail 行数");
                    ui.add(egui::DragValue::new(&mut f.tail).range(1..=100_000));
                    ui.end_row();
                }
                2 => {
                    ui.label("服务名");
                    ui.add(egui::TextEdit::singleline(&mut f.service).desired_width(220.0));
                    ui.end_row();
                    ui.label("since（可选）");
                    ui.add(egui::TextEdit::singleline(&mut f.since).desired_width(220.0))
                        .on_hover_text("journalctl 时间窗，如 today / 1h ago；留空不设");
                    ui.end_row();
                }
                3 => {
                    ui.label("命令");
                    ui.add(egui::TextEdit::singleline(&mut f.cmd).desired_width(220.0))
                        .on_hover_text("远端执行的完整命令，输出按行解析");
                    ui.end_row();
                }
                _ => {
                    ui.label("URL");
                    ui.add(egui::TextEdit::singleline(&mut f.url).desired_width(220.0));
                    ui.end_row();
                    ui.label("LogQL");
                    ui.add(egui::TextEdit::singleline(&mut f.query).desired_width(220.0));
                    ui.end_row();
                    ui.label("limit");
                    ui.add(egui::DragValue::new(&mut f.limit).range(1..=1_000_000));
                    ui.end_row();
                }
            }
        });
        self.form_actions(ui, |app| app.save_source_form(cfg));
    }

    fn save_source_form(&mut self, cfg: &Config) -> Result<(), String> {
        let f = self.manage.src.clone();
        let name = f.name.trim().to_string();
        if name.is_empty() {
            return Err("名称不能为空".into());
        }
        // 编辑模式下允许保持原名（同名 = 自己）；新增/改名撞已有名 → 拒绝
        let editing_same = match &self.manage.edit {
            Edit::Source(Some(idx)) => self
                .remote_cfg
                .as_ref()
                .and_then(|c| c.sources.get(*idx))
                .is_some_and(|s| s.name == name),
            _ => false,
        };
        if !editing_same && cfg.sources.iter().any(|s| s.name == name) {
            return Err(format!("源 {name} 已存在（名称是唯一键）"));
        }
        let kind = match f.kind_idx {
            0 => {
                if f.container.trim().is_empty() {
                    return Err("容器名不能为空".into());
                }
                SourceKindDraft::Docker {
                    container: f.container.trim().to_string(),
                    tail: f.tail.max(1),
                    follow: f.follow,
                    since: (!f.since.trim().is_empty()).then(|| f.since.trim().to_string()),
                }
            }
            1 => {
                if f.path.trim().is_empty() {
                    return Err("文件路径不能为空".into());
                }
                SourceKindDraft::File {
                    path: f.path.trim().to_string(),
                    tail: f.tail.max(1),
                }
            }
            2 => {
                if f.service.trim().is_empty() {
                    return Err("服务名不能为空".into());
                }
                SourceKindDraft::Journalctl {
                    service: f.service.trim().to_string(),
                    since: (!f.since.trim().is_empty()).then(|| f.since.trim().to_string()),
                }
            }
            3 => {
                if f.cmd.trim().is_empty() {
                    return Err("命令不能为空".into());
                }
                SourceKindDraft::Command {
                    cmd: f.cmd.trim().to_string(),
                }
            }
            4 => {
                if f.url.trim().is_empty() || f.query.trim().is_empty() {
                    return Err("URL 与 LogQL 不能为空".into());
                }
                SourceKindDraft::Loki {
                    url: f.url.trim().to_string(),
                    query: f.query.trim().to_string(),
                    limit: f.limit.max(1),
                }
            }
            _ => {
                if f.path.trim().is_empty() {
                    return Err("文件路径不能为空".into());
                }
                SourceKindDraft::LocalFile {
                    path: f.path.trim().to_string(),
                    tail: f.tail.max(1),
                }
            }
        };
        // 非 local 源必须引用已存在服务器
        let server = if f.kind_idx == 5 {
            String::new()
        } else {
            if f.server.is_empty() {
                return Err("先选择所属服务器（服务器在上方「＋ 新增」）".into());
            }
            if !cfg.servers.iter().any(|s| s.name == f.server) {
                return Err(format!("服务器 {} 不存在", f.server));
            }
            f.server.clone()
        };
        let draft = SourceDraft {
            name,
            server,
            kind,
        };
        self.write_config_transformed(|text| upsert_source(text, &draft), "源已保存")
    }

    // ---- 预设 ----

    fn manage_preset_list(&mut self, ui: &mut egui::Ui, cfg: &Config) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("预设").strong());
            if self.manage.edit == Edit::None && ui.small_button("＋ 新增").clicked() {
                self.manage.pst = PresetForm::default();
                self.manage.edit = Edit::Preset(None);
                self.manage.form_err = None;
            }
        });
        for (key, pc) in &cfg.presets {
            let mut parts = Vec::new();
            if let Some(l) = &pc.level {
                parts.push(format!("级别 {l}"));
            }
            if let Some(s) = &pc.since {
                parts.push(format!("近 {s}"));
            }
            let desc = if parts.is_empty() {
                key.clone()
            } else {
                parts.join(" · ")
            };
            ui.horizontal(|ui| {
                ui.label(format!("{key} · {}（{desc}）", pc.name.clone().unwrap_or_else(|| key.clone())));
                if self.manage.edit == Edit::None && ui.small_button("编辑").clicked() {
                    self.start_edit_preset(key);
                }
                self.delete_button(
                    ui,
                    DelTarget::Preset(key.clone()),
                    format!("删除预设 {key}"),
                );
            });
        }
        if cfg.presets.is_empty() {
            ui.label(RichText::new("（没有预设——连接源对话框底部一键应用 level+since）").weak());
        }
    }

    fn start_edit_preset(&mut self, key: &str) {
        let Some(pc) = self.remote_cfg.as_ref().and_then(|c| c.presets.get(key)) else {
            return;
        };
        let level_idx = pc
            .level
            .as_deref()
            .and_then(|l| LEVEL_OPTIONS.iter().position(|o| *o == l))
            .unwrap_or(0);
        self.manage.pst = PresetForm {
            key: key.to_string(),
            name: pc.name.clone().unwrap_or_default(),
            level_idx,
            since: pc.since.clone().unwrap_or_default(),
        };
        self.manage.edit = Edit::Preset(Some(key.to_string()));
        self.manage.form_err = None;
    }

    fn preset_form(&mut self, ui: &mut egui::Ui, key: Option<String>) {
        ui.label(
            RichText::new(if key.is_some() {
                format!("编辑预设 {}", key.as_deref().unwrap_or("?"))
            } else {
                "新增预设".to_string()
            })
            .strong(),
        );
        let f = &mut self.manage.pst;
        egui::Grid::new("pst_form").num_columns(2).show(ui, |ui| {
            ui.label("键名");
            ui.add(egui::TextEdit::singleline(&mut f.key).desired_width(220.0))
                .on_hover_text("config [presets] 下的键，唯一；编辑已有预设时不可改键名");
            ui.end_row();
            ui.label("显示名");
            ui.add(egui::TextEdit::singleline(&mut f.name).desired_width(220.0));
            ui.end_row();
            ui.label("级别");
            egui::ComboBox::from_id_salt("pst_level")
                .selected_text(LEVEL_OPTIONS[f.level_idx])
                .show_ui(ui, |ui| {
                    for (i, label) in LEVEL_OPTIONS.iter().enumerate() {
                        ui.selectable_value(&mut f.level_idx, i, *label);
                    }
                });
            ui.end_row();
            ui.label("since");
            ui.add(egui::TextEdit::singleline(&mut f.since).desired_width(220.0))
                .on_hover_text("时间窗：1h / 24h / today…；留空不设");
            ui.end_row();
        });
        self.form_actions(ui, |app| app.save_preset_form());
    }

    fn save_preset_form(&mut self) -> Result<(), String> {
        let f = self.manage.pst.clone();
        let key = f.key.trim().to_string();
        if key.is_empty() {
            return Err("键名不能为空".into());
        }
        // 编辑已有预设时键名不可改（改键名 = 新增 + 旧键残留）
        if let Edit::Preset(Some(orig)) = &self.manage.edit
            && *orig != key
        {
            return Err(format!(
                "编辑模式下键名不可改（原键 {orig}）；要改名请新增后删除旧键"
            ));
        }
        let level = (f.level_idx > 0).then(|| LEVEL_OPTIONS[f.level_idx].to_string());
        let draft = PresetDraft {
            key: key.clone(),
            name: (!f.name.trim().is_empty()).then(|| f.name.trim().to_string()),
            level,
            since: (!f.since.trim().is_empty()).then(|| f.since.trim().to_string()),
        };
        self.write_config_transformed(|text| upsert_preset(text, &draft), "预设已保存")
    }

    // ---- 公共动作 ----

    fn delete_button(&mut self, ui: &mut egui::Ui, target: DelTarget, tip: String) {
        // 编辑表单开着时不提供删除——删除会移动列表下标，令正在编辑的目标错位
        if self.manage.edit != Edit::None {
            return;
        }
        if self.manage.confirm_del.as_ref() == Some(&target) {
            ui.colored_label(ERR_FG, "确认删除？");
            if ui.small_button("确认").clicked() {
                let (label, res) = match &target {
                    DelTarget::Server(name) => (
                        "服务器已删除".into(),
                        self.transform_config(|t| remove_server(t, name)),
                    ),
                    DelTarget::Source(name) => (
                        "源已删除".into(),
                        self.transform_config(|t| remove_source(t, name)),
                    ),
                    DelTarget::Preset(key) => (
                        "预设已删除".into(),
                        self.transform_config(|t| remove_preset(t, key)),
                    ),
                };
                self.manage.confirm_del = None;
                self.status = match res {
                    Ok(()) => label,
                    Err(e) => e,
                };
            }
            if ui.small_button("取消").clicked() {
                self.manage.confirm_del = None;
            }
        } else if ui.small_button("删除").on_hover_text(tip).clicked() {
            self.manage.confirm_del = Some(target);
        }
    }

    /// 表单底部公共动作条（保存 / 取消 + 错误行）
    fn form_actions(&mut self, ui: &mut egui::Ui, save: impl FnOnce(&mut Self) -> Result<(), String>) {
        ui.horizontal(|ui| {
            if ui.button("保存").clicked() {
                match save(self) {
                    Ok(()) => self.manage.close_edit(),
                    Err(e) => self.manage.form_err = Some(e),
                }
            }
            if ui.button("取消").clicked() {
                self.manage.close_edit();
            }
        });
        if let Some(e) = &self.manage.form_err {
            ui.colored_label(ERR_FG, e);
        }
    }

    /// 读 config.toml 文本 → 变换 → 备份 + 写盘 + 重载缓存；Err 冒泡给表单/调用方
    fn transform_config(&mut self, f: impl FnOnce(&str) -> Result<String, String>) -> Result<(), String> {
        let path = logtail_core::config::config_dir().join("config.toml");
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let new = f(&text)?;
        if path.exists() {
            let bak = path.with_extension("toml.bak");
            if let Err(e) = std::fs::copy(&path, &bak) {
                return Err(format!("备份 config.toml.bak 失败: {e}"));
            }
        }
        std::fs::write(&path, new).map_err(|e| format!("config.toml 写入失败: {e}"))?;
        self.remote_cfg = None;
        self.remote_cfg_err = None;
        self.ensure_remote_cfg();
        Ok(())
    }

    /// transform_config + 成功状态栏提示；Err 冒泡（表单红字显示）
    fn write_config_transformed(
        &mut self,
        f: impl FnOnce(&str) -> Result<String, String>,
        ok_msg: &str,
    ) -> Result<(), String> {
        self.transform_config(f)?;
        self.status = format!("{ok_msg}（config.toml 已更新，备份在 config.toml.bak）");
        Ok(())
    }

    /// [v0.2.4] 设置菜单：remember_last_source 开关
    pub(crate) fn set_remember_last_source(&mut self, on: bool) {
        self.ensure_remote_cfg();
        if self.remote_cfg.is_none() {
            self.status = "config.toml 加载失败，无法修改该开关".into();
            return;
        }
        match self.transform_config(|text| set_remember_last_source(text, on)) {
            Ok(()) => self.status = format!("启动恢复上次激活源：{}（已写入 config.toml）", if on { "开" } else { "关" }),
            Err(e) => self.status = e,
        }
    }
}

fn source_type_label(src: &logtail_core::source::LogSource) -> &'static str {
    match src {
        logtail_core::source::LogSource::Docker { .. } => "docker",
        logtail_core::source::LogSource::File { .. } => "file",
        logtail_core::source::LogSource::Journalctl { .. } => "journalctl",
        logtail_core::source::LogSource::Command { .. } => "command",
        logtail_core::source::LogSource::Loki { .. } => "loki",
        logtail_core::source::LogSource::LocalFile { .. } => "本地",
    }
}

// ---------- 单测：纯函数层往返与注释保留 ----------

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]

    use super::*;
    use logtail_core::config::AuthCfg;
    use logtail_core::source::LogSource;

    /// 形如用户真实文件的样例（带注释与排版）
    const SAMPLE: &str = r#"# logtail 配置（与 TUI 共用）
[general]
default_tail = 500
remember_last_source = true   # 退出记住激活源与模式，下次启动自动连

# 服务器列表
[[servers]]
name = "prod-01"
host = "1.2.3.4"
port = 22
user = "root"
auth = { type = "password", password = "pw1" }   # 明文，建议迁移密钥环

[[servers]]
name = "prod-02"
host = "5.6.7.8"
port = 22
user = "ops"
auth = { type = "keyring" }

[[sources]]
name = "prod-api"
server = "prod-01"
source = { type = "docker", container = "my-app", tail = 500, follow = true }

[presets.quick_error]
name = "仅ERROR(1h)"
level = "error"
since = "1h"
"#;

    #[test]
    fn 开关_改值且保留行尾注释() {
        let out = set_remember_last_source(SAMPLE, false).unwrap();
        assert!(out.contains("remember_last_source = false"), "{out}");
        assert!(out.contains("# 退出记住激活源与模式"), "行尾注释保留: {out}");
        assert!(out.contains("default_tail = 500"), "其余键不动");
        let cfg: Config = toml::from_str(&out).unwrap();
        assert!(!cfg.general.remember_last_source);
        // 再开回来
        let out2 = set_remember_last_source(&out, true).unwrap();
        let cfg2: Config = toml::from_str(&out2).unwrap();
        assert!(cfg2.general.remember_last_source);
    }

    #[test]
    fn 开关_缺general段时自动创建() {
        let out = set_remember_last_source("", false).unwrap();
        let cfg: Config = toml::from_str(&out).unwrap();
        assert!(!cfg.general.remember_last_source);
    }

    #[test]
    fn 服务器_新增_编辑_删除() {
        // 新增 keyring 服务器（sudo + sudo_password）
        let d = ServerDraft {
            name: "staging".into(),
            host: "9.9.9.9".into(),
            port: 2222,
            user: "deploy".into(),
            auth: AuthDraft::Keyring,
            sudo: true,
            sudo_password: Some("sudopw".into()),
        };
        let out = upsert_server(SAMPLE, &d).unwrap();
        let cfg: Config = toml::from_str(&out).unwrap();
        assert_eq!(cfg.servers.len(), 3);
        let s = cfg.servers.iter().find(|s| s.name == "staging").unwrap();
        assert_eq!(s.port, 2222);
        assert!(matches!(s.auth, AuthCfg::Keyring { .. }));
        assert!(s.sudo && s.sudo_password.as_deref() == Some("sudopw"));
        assert!(out.contains("# 服务器列表"), "注释保留");

        // 编辑现有（改端口 + 换认证），其余条目不动
        let d2 = ServerDraft {
            name: "prod-01".into(),
            host: "1.2.3.4".into(),
            port: 2200,
            user: "root".into(),
            auth: AuthDraft::Key {
                path: "~/.ssh/id_ed25519".into(),
                passphrase: None,
            },
            sudo: false,
            sudo_password: None,
        };
        let out2 = upsert_server(SAMPLE, &d2).unwrap();
        let cfg2: Config = toml::from_str(&out2).unwrap();
        let p = cfg2.servers.iter().find(|s| s.name == "prod-01").unwrap();
        assert_eq!(p.port, 2200);
        assert!(matches!(
            &p.auth,
            AuthCfg::Key {
                path,
                passphrase: None
            } if path == "~/.ssh/id_ed25519"
        ));
        assert_eq!(cfg2.servers.len(), 2, "不重复新增");
        let q = cfg2.servers.iter().find(|s| s.name == "prod-02").unwrap();
        assert!(matches!(q.auth, AuthCfg::Keyring { .. }), "其余条目原样");

        // 删除
        let out3 = remove_server(&out2, "prod-01").unwrap();
        let cfg3: Config = toml::from_str(&out3).unwrap();
        assert_eq!(cfg3.servers.len(), 1);
        assert!(remove_server(SAMPLE, "no-such").is_err());
    }

    #[test]
    fn 源_新增_各类型_编辑_删除() {
        // docker 新增
        let d = SourceDraft {
            name: "staging-api".into(),
            server: "prod-02".into(),
            kind: SourceKindDraft::Docker {
                container: "api".into(),
                tail: 200,
                follow: true,
                since: None,
            },
        };
        let out = upsert_source(SAMPLE, &d).unwrap();
        let cfg: Config = toml::from_str(&out).unwrap();
        assert_eq!(cfg.sources.len(), 2);
        let s = cfg.sources.iter().find(|s| s.name == "staging-api").unwrap();
        assert!(matches!(
            &s.source,
            LogSource::Docker { container, tail, follow, .. }
                if container == "api" && *tail == 200 && *follow
        ));

        // 编辑现有为 local（server 键应被移除）
        let d2 = SourceDraft {
            name: "prod-api".into(),
            server: String::new(),
            kind: SourceKindDraft::LocalFile {
                path: "C:/logs/app.log".into(),
                tail: 300,
            },
        };
        let out2 = upsert_source(SAMPLE, &d2).unwrap();
        let cfg2: Config = toml::from_str(&out2).unwrap();
        let l = cfg2.sources.iter().find(|s| s.name == "prod-api").unwrap();
        assert!(matches!(
            &l.source,
            LogSource::LocalFile { path, tail } if path == "C:/logs/app.log" && *tail == 300
        ));
        assert!(l.server.is_empty(), "local 源 server 键移除");
        assert!(out2.contains("server = \"prod-02\"") == false || cfg2.sources.len() == 2);

        // journalctl + loki + command 往返
        for (name, kind) in [
            (
                "j",
                SourceKindDraft::Journalctl {
                    service: "nginx".into(),
                    since: Some("today".into()),
                },
            ),
            (
                "lk",
                SourceKindDraft::Loki {
                    url: "http://localhost:3100".into(),
                    query: "{job=\"x\"}".into(),
                    limit: 500,
                },
            ),
            ("cmd", SourceKindDraft::Command { cmd: "tail -f /x".into() }),
        ] {
            let d = SourceDraft {
                name: name.into(),
                server: "prod-01".into(),
                kind,
            };
            let o = upsert_source(SAMPLE, &d).unwrap();
            let c: Config = toml::from_str(&o).unwrap();
            assert!(c.sources.iter().any(|s| s.name == name));
        }

        // 删除（out = SAMPLE + staging-api）
        let out3 = remove_source(&out, "staging-api").unwrap();
        let cfg3: Config = toml::from_str(&out3).unwrap();
        assert_eq!(cfg3.sources.len(), 1);
        assert!(remove_source(SAMPLE, "no-such").is_err());
    }

    #[test]
    fn 预设_新增_编辑_删除_空值键不写() {
        let d = PresetDraft {
            key: "warn_today".into(),
            name: Some("警告(今天)".into()),
            level: Some("warn".into()),
            since: Some("today".into()),
        };
        let out = upsert_preset(SAMPLE, &d).unwrap();
        let cfg: Config = toml::from_str(&out).unwrap();
        let pc = cfg.presets.get("warn_today").unwrap();
        assert_eq!(pc.level.as_deref(), Some("warn"));
        assert_eq!(pc.since.as_deref(), Some("today"));
        assert!(out.contains("[presets.warn_today]"), "子表头渲染");

        // 全空值 = 只有键
        let d2 = PresetDraft {
            key: "empty".into(),
            name: None,
            level: None,
            since: None,
        };
        let out2 = upsert_preset(&out, &d2).unwrap();
        let cfg2: Config = toml::from_str(&out2).unwrap();
        let pc2 = cfg2.presets.get("empty").unwrap();
        assert!(pc2.name.is_none() && pc2.level.is_none() && pc2.since.is_none());

        // 编辑：原键替换
        let d3 = PresetDraft {
            key: "warn_today".into(),
            name: Some("改名".into()),
            level: Some("info".into()),
            since: None,
        };
        let out3 = upsert_preset(&out2, &d3).unwrap();
        let cfg3: Config = toml::from_str(&out3).unwrap();
        let pc3 = cfg3.presets.get("warn_today").unwrap();
        assert_eq!(pc3.name.as_deref(), Some("改名"));
        assert_eq!(pc3.level.as_deref(), Some("info"));
        assert!(pc3.since.is_none(), "清空的键不残留");

        // 删除
        let out4 = remove_preset(&out3, "empty").unwrap();
        let cfg4: Config = toml::from_str(&out4).unwrap();
        assert!(cfg4.presets.get("empty").is_none());
        assert!(remove_preset(SAMPLE, "no-such").is_err());
    }

    #[test]
    fn 编辑现有条目_未管字段与注释尽量保留() {
        // 给 prod-01 表加一个未来字段注释，再编辑它——未管键应保留
        let sample = SAMPLE.replace(
            "auth = { type = \"password\", password = \"pw1\" }   # 明文，建议迁移密钥环",
            "auth = { type = \"password\", password = \"pw1\" }   # 明文\nfuture_key = 1 # 未来字段",
        );
        let d = ServerDraft {
            name: "prod-01".into(),
            host: "1.2.3.4".into(),
            port: 22,
            user: "root".into(),
            auth: AuthDraft::Password("new-pw".into()),
            sudo: false,
            sudo_password: None,
        };
        let out = upsert_server(&sample, &d).unwrap();
        assert!(out.contains("future_key = 1"), "未管键保留: {out}");
        let cfg: Config = toml::from_str(&out).unwrap();
        let s = cfg.servers.iter().find(|s| s.name == "prod-01").unwrap();
        assert!(matches!(
            s.auth,
            AuthCfg::Password { ref password } if password == "new-pw"
        ));
        // 未被编辑的 prod-02 行尾注释仍在
        assert!(out.contains("# 明文"), "行尾注释尽量保留");
    }

    #[test]
    fn 服务器缺段时新增创建数组表() {
        let out = upsert_server(
            "",
            &ServerDraft {
                name: "solo".into(),
                host: "h".into(),
                port: 22,
                user: "u".into(),
                auth: AuthDraft::Password("p".into()),
                sudo: false,
                sudo_password: None,
            },
        )
        .unwrap();
        let cfg: Config = toml::from_str(&out).unwrap();
        assert_eq!(cfg.servers.len(), 1);
    }

    // 供 trait 空实现编译检查（无实际逻辑）
    #[test]
    fn 纯函数层不依赖窗口状态() {
        assert!(parse_doc("a = 1").is_ok());
        assert!(parse_doc("[").is_err());
    }
}
