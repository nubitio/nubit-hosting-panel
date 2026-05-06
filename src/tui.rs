#![allow(clippy::collapsible_if)]

mod selection;

use std::{
    fs, io,
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    sync::mpsc,
};

use color_eyre::eyre::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Tabs},
};

use std::collections::HashMap;

use crate::{
    backup, caddy,
    config::Config,
    db, docker, doctor, ssh,
    store::{
        App as HostingApp, Client, DatabaseGrant, DbServer, DomainAlias, SshKey, SshUser, Store,
    },
};

// ── System info ───────────────────────────────────────────────────────────────

#[derive(Default, Clone)]
struct SysInfo {
    hostname: String,
    primary_ip: String,
    cpu_usage_pct: f32,
    ram_used: u64,
    ram_total: u64,
    disk_used: u64,
    disk_total: u64,
    load_avg: String,
}

fn load_sys_info() -> SysInfo {
    use sysinfo::{Disks, System};
    let mut sys = System::new();
    sys.refresh_cpu_usage();
    sys.refresh_memory();
    let hostname = System::host_name().unwrap_or_else(|| "unknown".to_string());
    let cpu_usage = sys.global_cpu_usage();
    let ram_used = sys.used_memory();
    let ram_total = sys.total_memory();
    let disks = Disks::new_with_refreshed_list();
    let (disk_used, disk_total) = disks
        .iter()
        .find(|d| d.mount_point() == std::path::Path::new("/"))
        .map(|d| (d.total_space() - d.available_space(), d.total_space()))
        .unwrap_or((0, 0));
    let load = System::load_average();
    let load_avg = format!("{:.2} {:.2} {:.2}", load.one, load.five, load.fifteen);
    let primary_ip = std::net::UdpSocket::bind("0.0.0.0:0")
        .ok()
        .and_then(|s| {
            s.connect("8.8.8.8:80").ok()?;
            s.local_addr().ok()
        })
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    SysInfo {
        hostname,
        primary_ip,
        cpu_usage_pct: cpu_usage,
        ram_used,
        ram_total,
        disk_used,
        disk_total,
        load_avg,
    }
}

fn fmt_bytes(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    }
}

fn pct_bar(used: u64, total: u64, width: usize) -> (String, Color) {
    let pct = if total == 0 {
        0.0
    } else {
        used as f64 / total as f64 * 100.0
    };
    let filled = (pct / 100.0 * width as f64).round() as usize;
    let bar = format!(
        "[{}{}] {:.0}%",
        "█".repeat(filled.min(width)),
        "░".repeat(width.saturating_sub(filled)),
        pct
    );
    let color = if pct >= 90.0 {
        Color::Red
    } else if pct >= 75.0 {
        Color::Yellow
    } else {
        Color::Green
    };
    (bar, color)
}

// ── Tabs ──────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum ActiveTab {
    Dashboard,
    Clients,
    Apps,
    Databases,
    Backups,
    Ssh,
}

impl ActiveTab {
    fn index(self) -> usize {
        match self {
            Self::Dashboard => 0,
            Self::Clients => 1,
            Self::Apps => 2,
            Self::Databases => 3,
            Self::Backups => 4,
            Self::Ssh => 5,
        }
    }
    fn from_index(i: usize) -> Self {
        match i {
            1 => Self::Clients,
            2 => Self::Apps,
            3 => Self::Databases,
            4 => Self::Backups,
            5 => Self::Ssh,
            _ => Self::Dashboard,
        }
    }
    fn next(self) -> Self {
        Self::from_index((self.index() + 1) % 6)
    }
    fn prev(self) -> Self {
        Self::from_index((self.index() + 5) % 6)
    }
}

// ── SSH tab focus ────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum SshFocus {
    #[default]
    Users,
    Keys,
}

// ── Apps tab focus ────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum AppFocus {
    #[default]
    Apps,
    Aliases,
}

// ── DB tab focus ──────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum DbFocus {
    #[default]
    Servers,
    Grants,
}

// ── Form / Modal ──────────────────────────────────────────────────────────────

#[derive(Default, Clone)]
struct Input {
    pub value: String,
}

impl Input {
    fn push(&mut self, c: char) {
        self.value.push(c);
    }
    fn pop(&mut self) {
        self.value.pop();
    }
}

struct FormField {
    label: &'static str,
    input: Input,
    required: bool,
    placeholder: &'static str,
    options: Vec<String>,
}

impl FormField {
    fn req(label: &'static str, placeholder: &'static str) -> Self {
        Self {
            label,
            input: Input::default(),
            required: true,
            placeholder,
            options: vec![],
        }
    }
    fn opt(label: &'static str, placeholder: &'static str) -> Self {
        Self {
            label,
            input: Input::default(),
            required: false,
            placeholder,
            options: vec![],
        }
    }
    fn prefill(mut self, v: &str) -> Self {
        self.input.value = v.to_string();
        self
    }
    fn select(label: &'static str, options: Vec<String>) -> Self {
        let value = options.first().cloned().unwrap_or_default();
        Self {
            label,
            input: Input { value },
            required: true,
            placeholder: "",
            options,
        }
    }
    fn is_selector(&self) -> bool {
        !self.options.is_empty()
    }
    fn cycle_next(&mut self) {
        if self.options.is_empty() {
            return;
        }
        let idx = self
            .options
            .iter()
            .position(|o| *o == self.input.value)
            .unwrap_or(0);
        self.input.value = self.options[(idx + 1) % self.options.len()].clone();
    }
    fn cycle_prev(&mut self) {
        if self.options.is_empty() {
            return;
        }
        let len = self.options.len();
        let idx = self
            .options
            .iter()
            .position(|o| *o == self.input.value)
            .unwrap_or(0);
        self.input.value = self.options[(idx + len - 1) % len].clone();
    }
}

#[derive(Clone, Copy)]
enum FormKind {
    AddClient,
    AddApp,
    AddDbServer,
    Provision,
    Backup,
    ResetPassword,
    ReassignGrant,
    EditClient,
    EditApp,
    AddSshUser,
    AddSshKey,
    EditSshUser,
    AddAlias,
}

#[derive(Clone, Copy)]
enum ConfirmKind {
    DeleteClient,
    DeleteApp,
    RestoreBackup,
    CaddyApply,
    DeleteSshUser,
    DeleteSshKey,
    DeleteAlias,
}

enum Modal {
    None,
    Form {
        title: &'static str,
        fields: Vec<FormField>,
        focus: usize,
        kind: FormKind,
        error: Option<String>,
        /// ID de la entidad al editar (vacío al crear)
        payload: String,
    },
    Confirm {
        message: String,
        kind: ConfirmKind,
        payload: String,
    },
    Result {
        title: String,
        lines: Vec<String>,
    },
    Logs(LogsModal),
    Shell(ShellModal),
}

struct LogsModal {
    title: String,
    container: String,
    lines: Vec<String>,
    query: String,
    filter_active: bool,
    follow: bool,
    paused: bool,
    rx: mpsc::Receiver<String>,
    child: Option<std::process::Child>,
}

struct ShellModal {
    title: String,
    output: Vec<String>,
    rx: mpsc::Receiver<String>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

// ── Backup entry ──────────────────────────────────────────────────────────────

#[derive(Clone)]
struct BackupEntry {
    server: String,
    database: String,
    filename: String,
    size_bytes: u64,
    path: PathBuf,
}

fn format_size(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn scan_backups(dir: &std::path::Path) -> Vec<BackupEntry> {
    backup::list_backups(dir, None, None)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|path| {
            let meta = fs::metadata(&path).ok()?;
            let filename = path.file_name()?.to_str()?.to_string();
            let rel = path.strip_prefix(dir).ok()?;
            let mut parts = rel.components();
            let server = parts.next()?.as_os_str().to_str()?.to_string();
            let database = parts.next()?.as_os_str().to_str()?.to_string();
            Some(BackupEntry {
                server,
                database,
                filename,
                size_bytes: meta.len(),
                path,
            })
        })
        .collect()
}

// ── State ─────────────────────────────────────────────────────────────────────

struct TuiState {
    tab: ActiveTab,
    clients: Vec<Client>,
    apps: Vec<HostingApp>,
    aliases: Vec<DomainAlias>,
    db_servers: Vec<DbServer>,
    grants: Vec<DatabaseGrant>,
    backups: Vec<BackupEntry>,
    ssh_users: Vec<SshUser>,
    ssh_keys: Vec<SshKey>,
    doctor_checks: Vec<doctor::Check>,
    doctor_loaded: bool,
    sys_info: SysInfo,
    last_sys_refresh: std::time::Instant,
    clients_table: TableState,
    apps_table: TableState,
    aliases_table: TableState,
    db_table: TableState,
    grants_table: TableState,
    backups_table: TableState,
    ssh_users_table: TableState,
    ssh_keys_table: TableState,
    db_focus: DbFocus,
    app_focus: AppFocus,
    ssh_focus: SshFocus,
    /// app_id → estado del contenedor Docker ("running", "exited", ...)
    container_status: HashMap<String, String>,
    /// Query de filtro activo (vacío = sin filtro)
    filter_query: String,
    filter_active: bool,
    status: String,
    modal: Modal,
}

impl TuiState {
    fn load(store: &Store, cfg: &Config) -> Result<Self> {
        let doctor_checks = doctor::run(cfg, store).unwrap_or_default();
        let apps = store.list_apps()?;
        let mut s = Self {
            tab: ActiveTab::Dashboard,
            clients: store.list_clients()?,
            aliases: store.list_domain_aliases()?,
            db_servers: store.list_db_servers()?,
            grants: store.list_database_grants()?,
            backups: scan_backups(&cfg.backup_dir),
            ssh_users: store.list_ssh_users()?,
            ssh_keys: store.list_ssh_keys()?,
            doctor_checks,
            doctor_loaded: true,
            sys_info: load_sys_info(),
            last_sys_refresh: std::time::Instant::now(),
            clients_table: TableState::default(),
            apps_table: TableState::default(),
            aliases_table: TableState::default(),
            db_table: TableState::default(),
            grants_table: TableState::default(),
            backups_table: TableState::default(),
            ssh_users_table: TableState::default(),
            ssh_keys_table: TableState::default(),
            db_focus: DbFocus::Servers,
            app_focus: AppFocus::Apps,
            ssh_focus: SshFocus::Users,
            container_status: HashMap::new(),
            filter_query: String::new(),
            filter_active: false,
            status: String::new(),
            modal: Modal::None,
            apps,
        };
        s.refresh_container_status();
        Ok(s)
    }

    fn reload(&mut self, store: &Store, cfg: &Config) -> Result<()> {
        self.clients = store.list_clients()?;
        self.apps = store.list_apps()?;
        self.aliases = store.list_domain_aliases()?;
        self.db_servers = store.list_db_servers()?;
        self.grants = store.list_database_grants()?;
        self.backups = scan_backups(&cfg.backup_dir);
        self.ssh_users = store.list_ssh_users()?;
        self.ssh_keys = store.list_ssh_keys()?;
        self.doctor_checks = doctor::run(cfg, store).unwrap_or_default();
        self.doctor_loaded = true;
        self.sys_info = load_sys_info();
        self.last_sys_refresh = std::time::Instant::now();
        self.refresh_container_status();
        Ok(())
    }

    fn switch_tab(&mut self, tab: ActiveTab) {
        self.tab = tab;
        self.status.clear();
        self.filter_query.clear();
        self.filter_active = false;
    }

    /// Actualiza el estado de contenedores Docker parseando el upstream de cada app.
    fn refresh_container_status(&mut self) {
        self.container_status.clear();
        for app in &self.apps {
            let Some(host) = docker::container_name_from_upstream(&app.upstream) else {
                continue;
            };
            if let Ok(out) = std::process::Command::new("docker")
                .args(["inspect", "--format", "{{.State.Status}}", &host])
                .output()
            {
                if out.status.success() {
                    let status = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    self.container_status.insert(app.id.clone(), status);
                }
            }
        }
    }

    // ── Filtered views (aplica filter_query) ─────────────────────────────

    fn filtered_clients(&self) -> Vec<&Client> {
        let q = self.filter_query.to_lowercase();
        self.clients
            .iter()
            .filter(|c| {
                q.is_empty()
                    || c.slug.to_lowercase().contains(&q)
                    || c.name.to_lowercase().contains(&q)
                    || c.email.as_deref().unwrap_or("").to_lowercase().contains(&q)
                    || c.notes.as_deref().unwrap_or("").to_lowercase().contains(&q)
            })
            .collect()
    }

    fn selected_client(&self) -> Option<&Client> {
        let clients = self.filtered_clients();
        selection::selected(&clients, self.clients_table.selected())
    }

    fn filtered_apps_view(&self) -> Vec<&HostingApp> {
        let q = self.filter_query.to_lowercase();
        self.apps
            .iter()
            .filter(|a| {
                q.is_empty()
                    || a.client_slug.to_lowercase().contains(&q)
                    || a.slug.to_lowercase().contains(&q)
                    || a.domain.to_lowercase().contains(&q)
                    || a.upstream.to_lowercase().contains(&q)
                    || a.notes.as_deref().unwrap_or("").to_lowercase().contains(&q)
            })
            .collect()
    }

    fn filtered_ssh_users_view(&self) -> Vec<&SshUser> {
        let q = self.filter_query.to_lowercase();
        self.ssh_users
            .iter()
            .filter(|u| {
                q.is_empty()
                    || u.username.to_lowercase().contains(&q)
                    || u.client_slug.to_lowercase().contains(&q)
                    || u.app_slug
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&q)
            })
            .collect()
    }

    fn nav_down(&mut self) {
        match (self.tab, self.db_focus) {
            (ActiveTab::Clients, _) => {
                let len = self.filtered_clients().len();
                nav_down(&mut self.clients_table, len);
            }
            (ActiveTab::Apps, _) => match self.app_focus {
                AppFocus::Apps => {
                    let len = self.filtered_apps_view().len();
                    nav_down(&mut self.apps_table, len);
                }
                AppFocus::Aliases => {
                    let len = self.filtered_aliases().len();
                    nav_down(&mut self.aliases_table, len);
                }
            },
            (ActiveTab::Databases, DbFocus::Servers) => {
                nav_down(&mut self.db_table, self.db_servers.len())
            }
            (ActiveTab::Databases, DbFocus::Grants) => {
                let len = self.filtered_grants().len();
                nav_down(&mut self.grants_table, len);
            }
            (ActiveTab::Backups, _) => nav_down(&mut self.backups_table, self.backups.len()),
            (ActiveTab::Ssh, _) => match self.ssh_focus {
                SshFocus::Users => {
                    let len = self.filtered_ssh_users_view().len();
                    nav_down(&mut self.ssh_users_table, len);
                }
                SshFocus::Keys => {
                    let len = self.filtered_ssh_keys().len();
                    nav_down(&mut self.ssh_keys_table, len);
                }
            },
            _ => {}
        }
    }

    fn nav_up(&mut self) {
        match (self.tab, self.db_focus) {
            (ActiveTab::Clients, _) => {
                let len = self.filtered_clients().len();
                nav_up(&mut self.clients_table, len);
            }
            (ActiveTab::Apps, _) => match self.app_focus {
                AppFocus::Apps => {
                    let len = self.filtered_apps_view().len();
                    nav_up(&mut self.apps_table, len);
                }
                AppFocus::Aliases => {
                    let len = self.filtered_aliases().len();
                    nav_up(&mut self.aliases_table, len);
                }
            },
            (ActiveTab::Databases, DbFocus::Servers) => {
                nav_up(&mut self.db_table, self.db_servers.len())
            }
            (ActiveTab::Databases, DbFocus::Grants) => {
                let len = self.filtered_grants().len();
                nav_up(&mut self.grants_table, len);
            }
            (ActiveTab::Backups, _) => nav_up(&mut self.backups_table, self.backups.len()),
            (ActiveTab::Ssh, _) => match self.ssh_focus {
                SshFocus::Users => {
                    let len = self.filtered_ssh_users_view().len();
                    nav_up(&mut self.ssh_users_table, len);
                }
                SshFocus::Keys => {
                    let len = self.filtered_ssh_keys().len();
                    nav_up(&mut self.ssh_keys_table, len);
                }
            },
            _ => {}
        }
    }

    fn filtered_grants(&self) -> Vec<&DatabaseGrant> {
        let selected = self
            .db_table
            .selected()
            .and_then(|i| self.db_servers.get(i))
            .map(|s| s.name.clone());
        self.grants
            .iter()
            .filter(|g| {
                selected
                    .as_deref()
                    .map(|n| g.server_name == n)
                    .unwrap_or(true)
            })
            .collect()
    }

    fn filtered_ssh_keys(&self) -> Vec<&SshKey> {
        let selected_user_id = self
            .ssh_users_table
            .selected()
            .and_then(|i| self.ssh_users.get(i))
            .map(|u| u.id.clone());
        self.ssh_keys
            .iter()
            .filter(|k| {
                selected_user_id
                    .as_deref()
                    .map(|id| k.user_id == id)
                    .unwrap_or(false)
            })
            .collect()
    }

    fn selected_ssh_user(&self) -> Option<&SshUser> {
        let users = self.filtered_ssh_users_view();
        selection::selected(&users, self.ssh_users_table.selected())
    }

    /// Recalcula las opciones del selector "App" basándose en el valor actual del campo Cliente.
    /// basándose en el valor actual del campo Cliente (campo 0).
    fn refresh_ssh_app_options(&mut self) {
        let is_ssh_user_form = if let Modal::Form { kind, .. } = &self.modal {
            matches!(
                kind,
                FormKind::AddSshUser | FormKind::EditSshUser | FormKind::ReassignGrant
            )
        } else {
            false
        };
        if !is_ssh_user_form {
            return;
        }

        let client_slug = if let Modal::Form { fields, .. } = &self.modal {
            fields
                .first()
                .map(|f| f.input.value.clone())
                .unwrap_or_default()
        } else {
            return;
        };

        let mut new_opts = vec!["(ninguna)".to_string()];
        new_opts.extend(
            self.apps
                .iter()
                .filter(|a| a.client_slug == client_slug)
                .map(|a| a.slug.clone()),
        );

        if let Modal::Form { fields, .. } = &mut self.modal {
            if let Some(app_field) = fields.last_mut() {
                if app_field.is_selector() {
                    let current = app_field.input.value.clone();
                    app_field.options = new_opts.clone();
                    if !new_opts.contains(&current) {
                        app_field.input.value = new_opts.first().cloned().unwrap_or_default();
                    }
                }
            }
        }
    }

    fn filtered_aliases(&self) -> Vec<&DomainAlias> {
        let selected_app_id = self
            .apps_table
            .selected()
            .and_then(|i| self.apps.get(i))
            .map(|a| a.id.clone());
        self.aliases
            .iter()
            .filter(|al| {
                selected_app_id
                    .as_deref()
                    .map(|id| al.app_id == id)
                    .unwrap_or(false)
            })
            .collect()
    }

    fn selected_app(&self) -> Option<&HostingApp> {
        let apps = self.filtered_apps_view();
        selection::selected(&apps, self.apps_table.selected())
    }

    fn selected_server(&self) -> Option<&DbServer> {
        self.db_table
            .selected()
            .and_then(|i| self.db_servers.get(i))
    }

    fn server_options(&self) -> Vec<String> {
        self.db_servers.iter().map(|s| s.name.clone()).collect()
    }

    fn client_options(&self) -> Vec<String> {
        self.clients.iter().map(|c| c.slug.clone()).collect()
    }

    /// Opciones de app para un cliente: "(ninguna)" + slugs filtrados.
    fn app_options_for_client(&self, client_slug: &str) -> Vec<String> {
        let mut opts = vec!["(ninguna)".to_string()];
        opts.extend(
            self.apps
                .iter()
                .filter(|a| a.client_slug == client_slug)
                .map(|a| a.slug.clone()),
        );
        opts
    }

    fn open_add(&mut self) {
        let server_opts = self.server_options();
        let client_opts = self.client_options();
        let prefill_server = self
            .selected_server()
            .map(|s| s.name.clone())
            .unwrap_or_default();
        let prefill_client = self
            .selected_client()
            .map(|c| c.slug.clone())
            .unwrap_or_default();

        match self.tab {
            ActiveTab::Clients => {
                self.modal = Modal::Form {
                    title: " Agregar Cliente ",
                    fields: vec![
                        FormField::req("Slug", "ej: acme-corp"),
                        FormField::req("Nombre", "ej: Acme Corp"),
                        FormField::opt("Email", "ej: ops@acme.com"),
                    ],
                    focus: 0,
                    kind: FormKind::AddClient,
                    error: None,
                    payload: String::new(),
                };
            }
            ActiveTab::Apps => {
                let prefill = if prefill_client.is_empty() {
                    "".to_string()
                } else {
                    prefill_client
                };
                match self.app_focus {
                    AppFocus::Apps => {
                        let mut client_field = if client_opts.is_empty() {
                            FormField::req("Cliente (slug)", "ej: acme-corp")
                        } else {
                            FormField::select("Cliente", client_opts)
                        };
                        if !prefill.is_empty() {
                            client_field = client_field.prefill(&prefill);
                        }
                        self.modal = Modal::Form {
                            title: " Agregar App ",
                            fields: vec![
                                client_field,
                                FormField::req("App slug", "ej: web"),
                                FormField::req("Dominio", "ej: acme.nubit.site"),
                                FormField::req("Upstream", "ej: container:8080"),
                            ],
                            focus: 0,
                            kind: FormKind::AddApp,
                            error: None,
                            payload: String::new(),
                        };
                    }
                    AppFocus::Aliases => {
                        if let Some(app) = self.selected_app() {
                            let app_id = app.id.clone();
                            let app_name = format!("{}/{}", app.client_slug, app.slug);
                            self.modal = Modal::Form {
                                title: " Agregar Dominio Alias ",
                                fields: vec![FormField::req(
                                    "Dominio",
                                    "ej: marriott.tttracking.com",
                                )],
                                focus: 0,
                                kind: FormKind::AddAlias,
                                error: None,
                                payload: app_id,
                            };
                            self.status = format!("agregando alias para {app_name}");
                        } else {
                            self.status =
                                "Selecciona una app primero (↑↓ en panel superior)".to_string();
                        }
                    }
                }
            }
            ActiveTab::Databases => {
                self.modal = Modal::Form {
                    title: " Agregar DB Server ",
                    fields: vec![
                        FormField::req("Nombre", "ej: mariadb"),
                        FormField::select("Kind", vec!["mariadb".to_string(), "mssql".to_string()]),
                        FormField::req("Host", "127.0.0.1").prefill("127.0.0.1"),
                        FormField::req("Port", "3306").prefill("3306"),
                    ],
                    focus: 0,
                    kind: FormKind::AddDbServer,
                    error: None,
                    payload: String::new(),
                };
            }
            ActiveTab::Backups => {
                let server_field = if server_opts.is_empty() {
                    FormField::req("DB Server", "ej: mariadb").prefill(&prefill_server)
                } else {
                    let mut f = FormField::select("DB Server", server_opts);
                    if !prefill_server.is_empty() {
                        f = f.prefill(&prefill_server);
                    }
                    f
                };
                self.modal = Modal::Form {
                    title: " Nuevo Backup ",
                    fields: vec![
                        server_field,
                        FormField::req("Database", "ej: acme_web_prod"),
                    ],
                    focus: 0,
                    kind: FormKind::Backup,
                    error: None,
                    payload: String::new(),
                };
            }
            ActiveTab::Ssh => {
                let shell_opts = ssh::SHELLS.iter().map(|s| s.to_string()).collect();
                match self.ssh_focus {
                    SshFocus::Users => {
                        let mut client_field = if client_opts.is_empty() {
                            FormField::req("Cliente", "ej: acme-corp")
                        } else {
                            FormField::select("Cliente", client_opts.clone())
                        };
                        if !prefill_client.is_empty() {
                            client_field = client_field.prefill(&prefill_client);
                        }
                        let app_opts = self.app_options_for_client(if prefill_client.is_empty() {
                            client_opts.first().map(String::as_str).unwrap_or("")
                        } else {
                            &prefill_client
                        });
                        self.modal = Modal::Form {
                            title: " Agregar Usuario SSH ",
                            fields: vec![
                                client_field,
                                FormField::req("Username", "ej: acme-deploy"),
                                FormField::select("Shell", shell_opts),
                                FormField::opt("Home Dir", "vacío = /home/{username}"),
                                FormField::select("App (opcional)", app_opts),
                            ],
                            focus: 0,
                            kind: FormKind::AddSshUser,
                            error: None,
                            payload: String::new(),
                        };
                    }
                    SshFocus::Keys => {
                        if let Some(user) = self.selected_ssh_user() {
                            let user_id = user.id.clone();
                            self.modal = Modal::Form {
                                title: " Agregar Clave SSH ",
                                fields: vec![
                                    FormField::req("Etiqueta", "ej: laptop, trabajo"),
                                    FormField::req("Clave pública", "ssh-ed25519 AAAA..."),
                                ],
                                focus: 0,
                                kind: FormKind::AddSshKey,
                                error: None,
                                payload: user_id,
                            };
                        } else {
                            self.status =
                                "Selecciona un usuario primero (↑↓ en panel superior)".to_string();
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn open_provision(&mut self) {
        let server_opts = self.server_options();
        let client_opts = self.client_options();
        let prefill_server = self
            .selected_server()
            .map(|s| s.name.clone())
            .unwrap_or_default();

        let server_field = if server_opts.is_empty() {
            FormField::req("DB Server", "ej: mariadb").prefill(&prefill_server)
        } else {
            let mut f = FormField::select("DB Server", server_opts);
            if !prefill_server.is_empty() {
                f = f.prefill(&prefill_server);
            }
            f
        };
        let client_field = if client_opts.is_empty() {
            FormField::req("Cliente (slug)", "ej: acme-corp")
        } else {
            FormField::select("Cliente", client_opts)
        };

        self.modal = Modal::Form {
            title: " Provisionar DB ",
            fields: vec![
                server_field,
                client_field,
                FormField::req("App (slug)", "ej: web"),
                FormField::select(
                    "Env",
                    vec!["prod".to_string(), "staging".to_string(), "dev".to_string()],
                ),
                FormField::opt("Password", "vacío = auto-generar"),
            ],
            focus: 0,
            kind: FormKind::Provision,
            error: None,
            payload: String::new(),
        };
    }

    fn open_reset_password(&mut self) {
        let grants = self.filtered_grants();
        let grant = self
            .grants_table
            .selected()
            .and_then(|i| grants.get(i))
            .copied();
        if let Some(grant) = grant {
            let server_opts = self.server_options();
            let server_field = if server_opts.is_empty() {
                FormField::req("DB Server", "ej: mariadb").prefill(&grant.server_name)
            } else {
                FormField::select("DB Server", server_opts).prefill(&grant.server_name)
            };
            self.modal = Modal::Form {
                title: " Reset Password ",
                fields: vec![
                    server_field,
                    FormField::req("Usuario", "username").prefill(&grant.username),
                    FormField::req("Host", "%").prefill(&grant.host),
                    FormField::opt("Password", "vacío = auto-generar"),
                ],
                focus: 0,
                kind: FormKind::ResetPassword,
                error: None,
                payload: String::new(),
            };
        } else {
            self.status = "Selecciona un grant en la tabla inferior primero".to_string();
        }
    }

    fn open_reassign_grant(&mut self) {
        let grants = self.filtered_grants();
        let grant = self
            .grants_table
            .selected()
            .and_then(|i| grants.get(i))
            .copied();
        if let Some(grant) = grant {
            let client_opts = self.client_options();
            let mut client_field = if client_opts.is_empty() {
                FormField::req("Cliente", "ej: acme-corp")
            } else {
                FormField::select("Cliente", client_opts)
            };
            client_field = client_field.prefill(&grant.client_slug);
            let mut app_field = FormField::select(
                "App (opcional)",
                self.app_options_for_client(&grant.client_slug),
            );
            if let Some(app) = &grant.app_slug {
                app_field = app_field.prefill(app);
            }
            self.modal = Modal::Form {
                title: " Reasignar Grant DB ",
                fields: vec![
                    client_field,
                    FormField::req("Env", "prod").prefill(&grant.env),
                    app_field,
                ],
                focus: 0,
                kind: FormKind::ReassignGrant,
                error: None,
                payload: grant.id.clone(),
            };
        } else {
            self.status = "Selecciona un grant en la tabla inferior primero".to_string();
        }
    }

    fn open_logs(&mut self) {
        if self.tab != ActiveTab::Apps || self.app_focus != AppFocus::Apps {
            self.status = "Selecciona una app con upstream de contenedor".to_string();
            return;
        }
        let Some(app) = self.selected_app() else {
            self.status = "Selecciona una app primero (↑↓)".to_string();
            return;
        };
        match spawn_logs_modal(app) {
            Ok(modal) => self.modal = Modal::Logs(modal),
            Err(e) => self.status = format!("logs: {e}"),
        }
    }

    fn open_shell(&mut self, area: Rect) {
        if self.tab != ActiveTab::Apps || self.app_focus != AppFocus::Apps {
            self.status = "Selecciona una app con upstream de contenedor".to_string();
            return;
        }
        let Some(app) = self.selected_app() else {
            self.status = "Selecciona una app primero (↑↓)".to_string();
            return;
        };
        match spawn_shell_modal(app, area) {
            Ok(modal) => self.modal = Modal::Shell(modal),
            Err(e) => self.status = format!("shell: {e}"),
        }
    }

    fn poll_modal_streams(&mut self) {
        match &mut self.modal {
            Modal::Logs(logs) => {
                if !logs.paused {
                    while let Ok(line) = logs.rx.try_recv() {
                        logs.lines.push(line);
                        if logs.lines.len() > 5_000 {
                            logs.lines.drain(0..1_000);
                        }
                    }
                }
            }
            Modal::Shell(shell) => {
                while let Ok(chunk) = shell.rx.try_recv() {
                    for line in chunk.replace('\r', "").split('\n') {
                        if !line.is_empty() {
                            shell.output.push(line.to_string());
                        }
                    }
                    if shell.output.len() > 5_000 {
                        shell.output.drain(0..1_000);
                    }
                }
            }
            _ => {}
        }
    }

    fn open_restore(&mut self) {
        if let Some(backup) = self
            .backups_table
            .selected()
            .and_then(|i| self.backups.get(i))
        {
            let msg = format!(
                "Restaurar '{}'\nen server '{}' database '{}'?\n\nEsto REEMPLAZARÁ la base de datos.",
                backup.filename, backup.server, backup.database
            );
            self.modal = Modal::Confirm {
                message: msg,
                kind: ConfirmKind::RestoreBackup,
                payload: backup.path.to_string_lossy().to_string(),
            };
        } else {
            self.status = "Selecciona un backup primero (↑↓)".to_string();
        }
    }

    fn open_delete(&mut self) {
        match self.tab {
            ActiveTab::Clients => {
                if let Some(client) = self.selected_client() {
                    self.modal = Modal::Confirm {
                        message: format!(
                            "Eliminar cliente '{}'?\nSe eliminarán también todas sus apps.",
                            client.slug
                        ),
                        kind: ConfirmKind::DeleteClient,
                        payload: client.id.clone(),
                    };
                }
            }
            ActiveTab::Apps => match self.app_focus {
                AppFocus::Apps => {
                    if let Some(app) = self.selected_app() {
                        let alias_count =
                            self.aliases.iter().filter(|al| al.app_id == app.id).count();
                        let alias_note = if alias_count > 0 {
                            format!(" ({alias_count} alias también serán eliminados)")
                        } else {
                            String::new()
                        };
                        self.modal = Modal::Confirm {
                            message: format!(
                                "Eliminar app '{}/{}'?{}",
                                app.client_slug, app.slug, alias_note
                            ),
                            kind: ConfirmKind::DeleteApp,
                            payload: app.id.clone(),
                        };
                    }
                }
                AppFocus::Aliases => {
                    let aliases = self.filtered_aliases();
                    if let Some(alias) = self
                        .aliases_table
                        .selected()
                        .and_then(|i| aliases.get(i))
                        .copied()
                    {
                        let app_name = self
                            .selected_app()
                            .map(|a| format!("{}/{}", a.client_slug, a.slug))
                            .unwrap_or_default();
                        self.modal = Modal::Confirm {
                            message: format!(
                                "Eliminar alias '{}' de '{}'?",
                                alias.domain, app_name
                            ),
                            kind: ConfirmKind::DeleteAlias,
                            payload: alias.id.clone(),
                        };
                    } else {
                        self.status = "Selecciona un alias primero (↑↓)".to_string();
                    }
                }
            },
            ActiveTab::Ssh => match self.ssh_focus {
                SshFocus::Users => {
                    if let Some(user) = self.selected_ssh_user() {
                        self.modal = Modal::Confirm {
                            message: format!(
                                "Eliminar usuario SSH '{}'?\nSe eliminarán sus claves del panel.\nEl home dir ({}) se conserva.\n\nEsto ejecuta userdel en el sistema.",
                                user.username, user.home_dir
                            ),
                            kind: ConfirmKind::DeleteSshUser,
                            payload: user.id.clone(),
                        };
                    } else {
                        self.status = "Selecciona un usuario primero (↑↓)".to_string();
                    }
                }
                SshFocus::Keys => {
                    let keys = self.filtered_ssh_keys();
                    if let Some(key) = self
                        .ssh_keys_table
                        .selected()
                        .and_then(|i| keys.get(i))
                        .copied()
                    {
                        let user_name = self
                            .selected_ssh_user()
                            .map(|u| u.username.as_str())
                            .unwrap_or("?");
                        self.modal = Modal::Confirm {
                            message: format!("Eliminar clave '{}' de '{}'?", key.label, user_name),
                            kind: ConfirmKind::DeleteSshKey,
                            payload: key.id.clone(),
                        };
                    } else {
                        self.status = "Selecciona una clave primero (↑↓)".to_string();
                    }
                }
            },
            _ => {}
        }
    }

    fn open_edit(&mut self) {
        let client_opts = self.client_options();
        match self.tab {
            ActiveTab::Clients => {
                if let Some(client) = self.selected_client() {
                    let id = client.id.clone();
                    self.modal = Modal::Form {
                        title: " Editar Cliente ",
                        fields: vec![
                            FormField::req("Slug", "ej: acme-corp").prefill(&client.slug),
                            FormField::req("Nombre", "ej: Acme Corp").prefill(&client.name),
                            FormField::opt("Email", "ej: ops@acme.com")
                                .prefill(&client.email.clone().unwrap_or_default()),
                            FormField::opt("Notas", "contacto, proveedor, observaciones...")
                                .prefill(&client.notes.clone().unwrap_or_default()),
                        ],
                        focus: 0,
                        kind: FormKind::EditClient,
                        error: None,
                        payload: id,
                    };
                } else {
                    self.status = "Selecciona un cliente primero (↑↓)".to_string();
                }
            }
            ActiveTab::Apps => match self.app_focus {
                AppFocus::Apps => {
                    if let Some(app) = self.selected_app() {
                        let id = app.id.clone();
                        let current_client = app.client_slug.clone();
                        let mut client_field = if client_opts.is_empty() {
                            FormField::req("Cliente (slug)", "ej: acme-corp")
                        } else {
                            FormField::select("Cliente", client_opts)
                        };
                        client_field = client_field.prefill(&current_client);
                        self.modal = Modal::Form {
                            title: " Editar App ",
                            fields: vec![
                                client_field,
                                FormField::req("App slug", "ej: web").prefill(&app.slug),
                                FormField::req("Dominio", "ej: acme.nubit.site")
                                    .prefill(&app.domain),
                                FormField::req("Upstream", "ej: container:8080")
                                    .prefill(&app.upstream),
                                FormField::opt("Notas", "observaciones internas...")
                                    .prefill(&app.notes.clone().unwrap_or_default()),
                            ],
                            focus: 0,
                            kind: FormKind::EditApp,
                            error: None,
                            payload: id,
                        };
                    } else {
                        self.status = "Selecciona una app primero (↑↓)".to_string();
                    }
                }
                AppFocus::Aliases => {
                    self.status =
                        "Los alias no se editan — elimínalo y agrega uno nuevo (d / a)".to_string();
                }
            },
            ActiveTab::Ssh => match self.ssh_focus {
                SshFocus::Users => {
                    if let Some(user) = self.selected_ssh_user() {
                        let id = user.id.clone();
                        let current_client = user.client_slug.clone();
                        let current_app = user.app_slug.clone();
                        let shell_opts = ssh::SHELLS.iter().map(|s| s.to_string()).collect();
                        let mut client_field = if client_opts.is_empty() {
                            FormField::req("Cliente", "ej: acme-corp")
                        } else {
                            FormField::select("Cliente", client_opts)
                        };
                        client_field = client_field.prefill(&current_client);
                        let app_opts = self.app_options_for_client(&current_client);
                        let mut app_field = FormField::select("App (opcional)", app_opts);
                        if let Some(ref a) = current_app {
                            app_field = app_field.prefill(a);
                        }
                        self.modal = Modal::Form {
                            title: " Editar Usuario SSH ",
                            fields: vec![
                                client_field,
                                FormField::select("Shell", shell_opts).prefill(&user.shell),
                                app_field,
                            ],
                            focus: 0,
                            kind: FormKind::EditSshUser,
                            error: None,
                            payload: id,
                        };
                    } else {
                        self.status = "Selecciona un usuario primero (↑↓)".to_string();
                    }
                }
                SshFocus::Keys => {
                    self.status =
                        "Las claves no se editan — elimínala y agrega una nueva".to_string();
                }
            },
            _ => {}
        }
    }
}

fn nav_down(state: &mut TableState, len: usize) {
    if len == 0 {
        return;
    }
    let next = state.selected().map(|i| (i + 1) % len).unwrap_or(0);
    state.select(Some(next));
}

fn nav_up(state: &mut TableState, len: usize) {
    if len == 0 {
        return;
    }
    let prev = state
        .selected()
        .map(|i| if i == 0 { len - 1 } else { i - 1 })
        .unwrap_or(0);
    state.select(Some(prev));
}

// ── Event handling ────────────────────────────────────────────────────────────

fn handle_filter_key(state: &mut TuiState, code: KeyCode) {
    match code {
        KeyCode::Esc => {
            state.filter_query.clear();
            state.filter_active = false;
            // Reset selecciones para evitar índice fuera de rango
            state.clients_table.select(None);
            state.apps_table.select(None);
            state.ssh_users_table.select(None);
        }
        KeyCode::Enter => {
            // Confirmar filtro pero mantenerlo activo
            state.filter_active = false;
        }
        KeyCode::Backspace => {
            state.filter_query.pop();
        }
        KeyCode::Char(c) => {
            state.filter_query.push(c);
        }
        _ => {}
    }
}

fn handle_main_key(
    state: &mut TuiState,
    code: KeyCode,
    store: &Store,
    cfg: &Config,
    area: Rect,
) -> Result<()> {
    match code {
        KeyCode::Tab => {
            if state.tab == ActiveTab::Databases {
                state.db_focus = match state.db_focus {
                    DbFocus::Servers => DbFocus::Grants,
                    DbFocus::Grants => DbFocus::Servers,
                };
            } else if state.tab == ActiveTab::Apps {
                state.app_focus = match state.app_focus {
                    AppFocus::Apps => AppFocus::Aliases,
                    AppFocus::Aliases => AppFocus::Apps,
                };
            } else if state.tab == ActiveTab::Ssh {
                state.ssh_focus = match state.ssh_focus {
                    SshFocus::Users => SshFocus::Keys,
                    SshFocus::Keys => SshFocus::Users,
                };
            } else {
                state.switch_tab(state.tab.next());
            }
        }
        KeyCode::BackTab => state.switch_tab(state.tab.prev()),
        KeyCode::Char('1') => state.switch_tab(ActiveTab::Dashboard),
        KeyCode::Char('2') => state.switch_tab(ActiveTab::Clients),
        KeyCode::Char('3') => state.switch_tab(ActiveTab::Apps),
        KeyCode::Char('4') => state.switch_tab(ActiveTab::Databases),
        KeyCode::Char('5') => state.switch_tab(ActiveTab::Backups),
        KeyCode::Char('6') => state.switch_tab(ActiveTab::Ssh),
        KeyCode::Down | KeyCode::Char('j') => state.nav_down(),
        KeyCode::Up | KeyCode::Char('k') => state.nav_up(),
        KeyCode::Char('a') => state.open_add(),
        KeyCode::Char('d') => state.open_delete(),
        KeyCode::Char('e') if state.tab == ActiveTab::Apps || state.tab == ActiveTab::Clients => {
            state.open_edit()
        }
        KeyCode::Char('e') if state.tab == ActiveTab::Databases => state.open_reset_password(),
        KeyCode::Char('m') if state.tab == ActiveTab::Databases => state.open_reassign_grant(),
        KeyCode::Char('c') if state.tab == ActiveTab::Apps || state.tab == ActiveTab::Dashboard => {
            state.modal = Modal::Confirm {
                message: format!(
                    "Aplicar Caddyfile y recargar Caddy?\n{} apps serán publicadas.",
                    state.apps.len()
                ),
                kind: ConfirmKind::CaddyApply,
                payload: String::new(),
            };
        }
        KeyCode::Char('l') if state.tab == ActiveTab::Apps => state.open_logs(),
        KeyCode::Char('s') if state.tab == ActiveTab::Apps => state.open_shell(area),
        KeyCode::Char('p') if state.tab == ActiveTab::Databases => state.open_provision(),
        KeyCode::Enter if state.tab == ActiveTab::Backups => state.open_restore(),
        KeyCode::Char('r') => {
            state.reload(store, cfg)?;
            let ok = state.doctor_checks.iter().filter(|c| c.ok).count();
            let fail = state.doctor_checks.iter().filter(|c| !c.ok).count();
            state.status = format!(
                "actualizado — clients:{} apps:{} doctor:{} ok/{} fail",
                state.clients.len(),
                state.apps.len(),
                ok,
                fail
            );
        }
        KeyCode::Char('/') => {
            state.filter_query.clear();
            state.filter_active = true;
            state.status.clear();
        }
        _ => {}
    }
    Ok(())
}

fn handle_form_key(state: &mut TuiState, code: KeyCode, store: &Store, cfg: &Config) -> Result<()> {
    match code {
        KeyCode::Esc => state.modal = Modal::None,
        KeyCode::Tab => {
            // Guardar foco anterior para detectar si salimos del campo Cliente
            let prev_focus = if let Modal::Form { focus, .. } = &state.modal {
                *focus
            } else {
                0
            };
            let is_ssh_form = if let Modal::Form { kind, .. } = &state.modal {
                matches!(kind, FormKind::AddSshUser | FormKind::EditSshUser)
            } else {
                false
            };
            if let Modal::Form { focus, fields, .. } = &mut state.modal {
                *focus = (*focus + 1) % fields.len();
            }
            // Si avanzamos desde el campo Cliente (0), refrescar apps
            if is_ssh_form && prev_focus == 0 {
                state.refresh_ssh_app_options();
            }
        }
        KeyCode::BackTab => {
            if let Modal::Form { focus, fields, .. } = &mut state.modal {
                *focus = (*focus + fields.len() - 1) % fields.len();
            }
        }
        // Selector cycling: ←→ or ↑↓
        KeyCode::Left | KeyCode::Up => {
            let (is_ssh_form, cur_focus) = if let Modal::Form { kind, focus, .. } = &state.modal {
                (
                    matches!(kind, FormKind::AddSshUser | FormKind::EditSshUser),
                    *focus,
                )
            } else {
                (false, 0)
            };
            if let Modal::Form { focus, fields, .. } = &mut state.modal {
                if fields[*focus].is_selector() {
                    fields[*focus].cycle_prev();
                }
            }
            if is_ssh_form && cur_focus == 0 {
                state.refresh_ssh_app_options();
            }
        }
        KeyCode::Right | KeyCode::Down => {
            let (is_ssh_form, cur_focus) = if let Modal::Form { kind, focus, .. } = &state.modal {
                (
                    matches!(kind, FormKind::AddSshUser | FormKind::EditSshUser),
                    *focus,
                )
            } else {
                (false, 0)
            };
            if let Modal::Form { focus, fields, .. } = &mut state.modal {
                if fields[*focus].is_selector() {
                    fields[*focus].cycle_next();
                }
            }
            if is_ssh_form && cur_focus == 0 {
                state.refresh_ssh_app_options();
            }
        }
        KeyCode::Char(c) => {
            if let Modal::Form {
                focus,
                fields,
                error,
                ..
            } = &mut state.modal
            {
                if !fields[*focus].is_selector() {
                    fields[*focus].input.push(c);
                    *error = None;
                }
            }
        }
        KeyCode::Backspace => {
            if let Modal::Form {
                focus,
                fields,
                error,
                ..
            } = &mut state.modal
            {
                if !fields[*focus].is_selector() {
                    fields[*focus].input.pop();
                    *error = None;
                }
            }
        }
        KeyCode::Enter => try_submit(state, store, cfg)?,
        _ => {}
    }
    Ok(())
}

fn handle_confirm_key(
    state: &mut TuiState,
    code: KeyCode,
    store: &Store,
    cfg: &Config,
) -> Result<()> {
    match code {
        KeyCode::Esc => state.modal = Modal::None,
        KeyCode::Enter => try_confirm(state, store, cfg)?,
        _ => {}
    }
    Ok(())
}

fn handle_result_key(state: &mut TuiState, code: KeyCode) {
    if matches!(code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q')) {
        state.modal = Modal::None;
    }
}

fn handle_logs_key(state: &mut TuiState, code: KeyCode) {
    if let Modal::Logs(logs) = &mut state.modal {
        if logs.filter_active {
            match code {
                KeyCode::Esc => {
                    logs.query.clear();
                    logs.filter_active = false;
                }
                KeyCode::Enter => logs.filter_active = false,
                KeyCode::Backspace => {
                    logs.query.pop();
                }
                KeyCode::Char(c) => logs.query.push(c),
                _ => {}
            }
            return;
        }
        match code {
            KeyCode::Esc | KeyCode::Char('q') => {
                if let Some(mut child) = logs.child.take() {
                    let _ = child.kill();
                }
                state.modal = Modal::None;
            }
            KeyCode::Char('/') => logs.filter_active = true,
            KeyCode::Char('p') => logs.paused = !logs.paused,
            KeyCode::Char('f') => logs.follow = !logs.follow,
            KeyCode::Char('c') => logs.lines.clear(),
            _ => {}
        }
    }
}

fn handle_shell_key(state: &mut TuiState, key: KeyEvent) -> Result<()> {
    if let Modal::Shell(shell) = &mut state.modal {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('q') {
            let _ = shell.child.kill();
            state.modal = Modal::None;
            return Ok(());
        }
        let bytes: Vec<u8> = match key.code {
            KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) => {
                match c.to_ascii_lowercase() {
                    'c' => vec![0x03],
                    'd' => vec![0x04],
                    'l' => vec![0x0c],
                    _ => return Ok(()),
                }
            }
            KeyCode::Char(c) => c.to_string().into_bytes(),
            KeyCode::Enter => b"\r".to_vec(),
            KeyCode::Backspace => vec![0x7f],
            KeyCode::Tab => b"\t".to_vec(),
            KeyCode::Left => b"\x1b[D".to_vec(),
            KeyCode::Right => b"\x1b[C".to_vec(),
            KeyCode::Up => b"\x1b[A".to_vec(),
            KeyCode::Down => b"\x1b[B".to_vec(),
            KeyCode::Esc => b"\x1b".to_vec(),
            _ => Vec::new(),
        };
        if !bytes.is_empty() {
            shell.writer.write_all(&bytes)?;
            shell.writer.flush()?;
        }
    }
    Ok(())
}

fn try_submit(state: &mut TuiState, store: &Store, cfg: &Config) -> Result<()> {
    let (kind, data, payload, first_empty) = match &state.modal {
        Modal::Form {
            kind,
            fields,
            payload,
            ..
        } => {
            let data: Vec<String> = fields
                .iter()
                .map(|f| f.input.value.trim().to_string())
                .collect();
            let first_empty = fields
                .iter()
                .enumerate()
                .find(|(i, f)| f.required && data[*i].is_empty())
                .map(|(i, f)| (i, f.label));
            (*kind, data, payload.clone(), first_empty)
        }
        _ => return Ok(()),
    };

    if let Some((idx, label)) = first_empty {
        if let Modal::Form { error, focus, .. } = &mut state.modal {
            *error = Some(format!("{} es requerido", label));
            *focus = idx;
        }
        return Ok(());
    }

    match kind {
        FormKind::AddClient => {
            let slug = data[0].clone();
            let name = data[1].clone();
            let email = if data[2].is_empty() {
                None
            } else {
                Some(data[2].as_str())
            };
            match store.add_client(&slug, &name, email) {
                Ok(c) => {
                    state.modal = Modal::None;
                    state.reload(store, cfg)?;
                    state.status = format!("✓ cliente '{}' creado", c.slug);
                    state.switch_tab(ActiveTab::Clients);
                }
                Err(e) => set_modal_error(&mut state.modal, e.to_string()),
            }
        }
        FormKind::AddApp => match store.add_app(&data[0], &data[1], &data[2], &data[3]) {
            Ok(a) => {
                state.modal = Modal::None;
                state.reload(store, cfg)?;
                state.status = format!("✓ app '{}/{}' creada", a.client_slug, a.slug);
                state.switch_tab(ActiveTab::Apps);
            }
            Err(e) => set_modal_error(&mut state.modal, e.to_string()),
        },
        FormKind::AddDbServer => {
            let port: u16 = data[3]
                .parse()
                .unwrap_or(if data[1] == "mssql" { 1433 } else { 3306 });
            match store.add_db_server(&data[0], &data[1], &data[2], port) {
                Ok(s) => {
                    state.modal = Modal::None;
                    state.reload(store, cfg)?;
                    state.status = format!(
                        "✓ db server '{}' ({}) agregado — define HOSTINGCTL_DB_{}_URL",
                        s.name,
                        s.kind,
                        s.name.to_ascii_uppercase().replace('-', "_")
                    );
                    state.switch_tab(ActiveTab::Databases);
                }
                Err(e) => set_modal_error(&mut state.modal, e.to_string()),
            }
        }
        FormKind::Provision => {
            let password = if data[4].is_empty() {
                rand_password()
            } else {
                data[4].clone()
            };
            match store.db_server(&data[0]) {
                Err(e) => set_modal_error(&mut state.modal, format!("server no encontrado: {e}")),
                Ok(server) => match db::provision(
                    cfg, store, &server, &data[1], &data[2], &data[3], "%", None, None, password,
                ) {
                    Ok(p) => {
                        state.reload(store, cfg)?;
                        state.modal = Modal::Result {
                            title: "✓ DB Provisionada".to_string(),
                            lines: vec![
                                format!("Database: {}", p.database),
                                format!("Usuario:  {}", p.username),
                                format!("Host:     {}", p.host),
                                String::new(),
                                format!("Password: {}", p.password),
                                String::new(),
                                "Guarda el password, no se mostrará de nuevo.".to_string(),
                            ],
                        };
                        state.switch_tab(ActiveTab::Databases);
                    }
                    Err(e) => set_modal_error(&mut state.modal, e.to_string()),
                },
            }
        }
        FormKind::Backup => match store.db_server(&data[0]) {
            Err(e) => set_modal_error(&mut state.modal, format!("server no encontrado: {e}")),
            Ok(server) => match backup::backup(cfg, &server, &data[1], &cfg.backup_dir) {
                Ok(path) => {
                    state.modal = Modal::None;
                    state.reload(store, cfg)?;
                    state.status = format!("✓ backup: {}", path.display());
                    state.switch_tab(ActiveTab::Backups);
                }
                Err(e) => set_modal_error(&mut state.modal, e.to_string()),
            },
        },
        FormKind::ResetPassword => {
            let password = if data[3].is_empty() {
                rand_password()
            } else {
                data[3].clone()
            };
            match store.db_server(&data[0]) {
                Err(e) => set_modal_error(&mut state.modal, format!("server no encontrado: {e}")),
                Ok(server) => match db::reset_password(cfg, &server, &data[1], &data[2], &password)
                {
                    Ok(()) => {
                        state.modal = Modal::Result {
                            title: "✓ Password Actualizado".to_string(),
                            lines: vec![
                                format!("Server:   {}", data[0]),
                                format!("Usuario:  {}", data[1]),
                                format!("Host:     {}", data[2]),
                                String::new(),
                                format!("Password: {}", password),
                                String::new(),
                                "Guarda el password, no se mostrará de nuevo.".to_string(),
                            ],
                        };
                    }
                    Err(e) => set_modal_error(&mut state.modal, e.to_string()),
                },
            }
        }
        FormKind::ReassignGrant => {
            let app_slug = if data[2] == "(ninguna)" || data[2].is_empty() {
                None
            } else {
                Some(data[2].as_str())
            };
            match store.reassign_database_grant(&payload, &data[0], app_slug, Some(&data[1])) {
                Ok(()) => {
                    state.modal = Modal::None;
                    state.reload(store, cfg)?;
                    state.status = format!("✓ grant reasignado a {}", data[0]);
                    state.switch_tab(ActiveTab::Databases);
                }
                Err(e) => set_modal_error(&mut state.modal, e.to_string()),
            }
        }
        FormKind::EditClient => {
            let email = if data[2].is_empty() {
                None
            } else {
                Some(data[2].as_str())
            };
            let notes = if data[3].is_empty() {
                None
            } else {
                Some(data[3].as_str())
            };
            match store.update_client(&payload, &data[0], &data[1], email, notes) {
                Ok(()) => {
                    state.modal = Modal::None;
                    state.reload(store, cfg)?;
                    state.status = format!("✓ cliente '{}' actualizado", data[0]);
                    state.switch_tab(ActiveTab::Clients);
                }
                Err(e) => set_modal_error(&mut state.modal, e.to_string()),
            }
        }
        FormKind::EditApp => {
            let notes = if data[4].is_empty() {
                None
            } else {
                Some(data[4].as_str())
            };
            match store.update_app(&payload, &data[0], &data[1], &data[2], &data[3], notes) {
                Ok(()) => {
                    state.modal = Modal::None;
                    state.reload(store, cfg)?;
                    state.status = format!("✓ app '{}/{}' actualizada", data[0], data[1]);
                    state.switch_tab(ActiveTab::Apps);
                }
                Err(e) => set_modal_error(&mut state.modal, e.to_string()),
            }
        }
        FormKind::AddAlias => {
            // payload = app_id
            match store.add_domain_alias(&payload, &data[0]) {
                Ok(alias) => {
                    state.modal = Modal::None;
                    state.reload(store, cfg)?;
                    state.status = format!("✓ alias '{}' agregado", alias.domain);
                    state.switch_tab(ActiveTab::Apps);
                }
                Err(e) => set_modal_error(&mut state.modal, e.to_string()),
            }
        }
        FormKind::AddSshUser => {
            let home_dir = if data[3].is_empty() {
                format!("/home/{}", data[1])
            } else {
                data[3].clone()
            };
            // data[4] = app slug ("(ninguna)" → None)
            let app_slug = if data[4] == "(ninguna)" || data[4].is_empty() {
                None
            } else {
                Some(data[4].as_str())
            };
            match ssh::create_user(&data[1], &data[2], &home_dir) {
                Err(e) => set_modal_error(&mut state.modal, format!("useradd: {e}")),
                Ok(()) => {
                    match store.add_ssh_user(&data[1], &data[0], &data[2], &home_dir, app_slug) {
                        Ok(u) => {
                            state.modal = Modal::None;
                            state.reload(store, cfg)?;
                            state.status = format!("✓ usuario SSH '{}' creado", u.username);
                            state.switch_tab(ActiveTab::Ssh);
                        }
                        Err(e) => {
                            let _ = ssh::delete_user(&data[1]);
                            set_modal_error(&mut state.modal, e.to_string());
                        }
                    }
                }
            }
        }
        FormKind::AddSshKey => {
            // payload = user_id
            let user_info = state
                .ssh_users
                .iter()
                .find(|u| u.id == payload)
                .map(|u| (u.username.clone(), u.home_dir.clone()));
            match store.add_ssh_key(&payload, &data[0], &data[1]) {
                Ok(_) => {
                    state.modal = Modal::None;
                    state.reload(store, cfg)?;
                    let status = if let Some((uname, home)) = user_info {
                        let keys = store.keys_for_user(&payload).unwrap_or_default();
                        match ssh::sync_authorized_keys(&uname, &home, &keys) {
                            Ok(()) => format!("✓ clave '{}' agregada a '{}'", data[0], uname),
                            Err(e) => format!("✓ clave guardada, sync falló: {e}"),
                        }
                    } else {
                        format!("✓ clave '{}' agregada", data[0])
                    };
                    state.status = status;
                    state.switch_tab(ActiveTab::Ssh);
                }
                Err(e) => set_modal_error(&mut state.modal, e.to_string()),
            }
        }
        FormKind::EditSshUser => {
            // payload = user_id; data[0]=client, data[1]=shell, data[2]=app
            let old_shell_and_name = state
                .ssh_users
                .iter()
                .find(|u| u.id == payload)
                .map(|u| (u.shell.clone(), u.username.clone()));
            let app_slug = if data[2] == "(ninguna)" || data[2].is_empty() {
                None
            } else {
                Some(data[2].as_str())
            };
            match store.update_ssh_user(&payload, &data[0], &data[1], app_slug) {
                Ok(()) => {
                    if let Some((old_shell, uname)) = old_shell_and_name {
                        if old_shell != data[1] {
                            if let Err(e) = ssh::set_shell(&uname, &data[1]) {
                                state.modal = Modal::None;
                                state.reload(store, cfg)?;
                                state.status = format!("✓ DB ok, usermod falló: {e}");
                                return Ok(());
                            }
                        }
                    }
                    state.modal = Modal::None;
                    state.reload(store, cfg)?;
                    state.status = "✓ usuario SSH actualizado".to_string();
                    state.switch_tab(ActiveTab::Ssh);
                }
                Err(e) => set_modal_error(&mut state.modal, e.to_string()),
            }
        }
    }
    Ok(())
}

fn try_confirm(state: &mut TuiState, store: &Store, cfg: &Config) -> Result<()> {
    let (kind, payload) = match &state.modal {
        Modal::Confirm { kind, payload, .. } => (*kind, payload.clone()),
        _ => return Ok(()),
    };
    match kind {
        ConfirmKind::DeleteClient => {
            store.delete_client(&payload)?;
            state.modal = Modal::None;
            state.reload(store, cfg)?;
            state.clients_table.select(None);
            state.status = "✓ cliente eliminado".to_string();
        }
        ConfirmKind::DeleteApp => {
            store.delete_app(&payload)?;
            state.modal = Modal::None;
            state.reload(store, cfg)?;
            state.apps_table.select(None);
            state.aliases_table.select(None);
            state.status = "✓ app eliminada".to_string();
        }
        ConfirmKind::DeleteAlias => {
            store.delete_domain_alias(&payload)?;
            state.modal = Modal::None;
            state.reload(store, cfg)?;
            state.aliases_table.select(None);
            state.status = "✓ alias eliminado".to_string();
        }
        ConfirmKind::DeleteSshUser => {
            // Extraer info antes de borrar
            let username = state
                .ssh_users
                .iter()
                .find(|u| u.id == payload)
                .map(|u| u.username.clone());
            store.delete_ssh_user(&payload)?;
            state.modal = Modal::None;
            state.reload(store, cfg)?;
            state.ssh_users_table.select(None);
            state.ssh_keys_table.select(None);
            let mut msg = "✓ usuario SSH eliminado del panel".to_string();
            if let Some(uname) = username {
                if let Err(e) = ssh::delete_user(&uname) {
                    msg = format!("✓ eliminado del panel (userdel: {e})");
                }
            }
            state.status = msg;
        }
        ConfirmKind::DeleteSshKey => {
            // Extraer user_id y home_dir antes de borrar
            let user_info = state
                .ssh_keys
                .iter()
                .find(|k| k.id == payload)
                .and_then(|k| state.ssh_users.iter().find(|u| u.id == k.user_id))
                .map(|u| (u.id.clone(), u.username.clone(), u.home_dir.clone()));
            store.delete_ssh_key(&payload)?;
            state.modal = Modal::None;
            state.reload(store, cfg)?;
            state.ssh_keys_table.select(None);
            let mut msg = "✓ clave SSH eliminada".to_string();
            if let Some((uid, uname, home)) = user_info {
                let remaining = store.keys_for_user(&uid).unwrap_or_default();
                if let Err(e) = ssh::sync_authorized_keys(&uname, &home, &remaining) {
                    msg = format!("✓ clave eliminada, sync falló: {e}");
                }
            }
            state.status = msg;
        }
        ConfirmKind::CaddyApply => match caddy::apply(cfg, &state.apps, &state.aliases, true) {
            Ok(()) => {
                state.modal = Modal::Result {
                    title: "✓ Caddy Aplicado".to_string(),
                    lines: vec![
                        format!(
                            "{} apps ({} aliases) aplicados.",
                            state.apps.len(),
                            state.aliases.len()
                        ),
                        String::new(),
                        format!("Managed: {}", cfg.caddy_managed_path.display()),
                    ],
                };
            }
            Err(e) => {
                state.modal = Modal::None;
                state.status = format!("error caddy apply: {e}");
            }
        },
        ConfirmKind::RestoreBackup => {
            let path = std::path::Path::new(&payload);
            let rel = path.strip_prefix(&cfg.backup_dir).unwrap_or(path);
            let mut parts = rel.components();
            let server_name = parts
                .next()
                .and_then(|c| c.as_os_str().to_str())
                .unwrap_or("")
                .to_string();
            let database = parts
                .next()
                .and_then(|c| c.as_os_str().to_str())
                .unwrap_or("")
                .to_string();
            match store.db_server(&server_name) {
                Err(e) => {
                    state.modal = Modal::None;
                    state.status = format!("error: server no encontrado: {e}");
                }
                Ok(server) => match backup::restore(cfg, &server, &database, path) {
                    Ok(()) => {
                        state.modal = Modal::None;
                        state.status = format!("✓ restore aplicado: {database}");
                    }
                    Err(e) => {
                        state.modal = Modal::None;
                        state.status = format!("error restore: {e}");
                    }
                },
            }
        }
    }
    Ok(())
}

fn set_modal_error(modal: &mut Modal, err: String) {
    if let Modal::Form { error, .. } = modal {
        *error = Some(err);
    }
}

fn rand_password() -> String {
    use rand::distr::{Alphanumeric, SampleString};
    Alphanumeric.sample_string(&mut rand::rng(), 32)
}

fn spawn_logs_modal(app: &HostingApp) -> Result<LogsModal> {
    let container = docker::container_name_from_upstream(&app.upstream)
        .ok_or_else(|| color_eyre::eyre::eyre!("app no apunta a contenedor local"))?;
    let mut child = docker::spawn_logs(&container, 300, true)?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let (tx, rx) = mpsc::channel();
    if let Some(out) = stdout {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });
    }
    if let Some(err) = stderr {
        std::thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });
    }
    Ok(LogsModal {
        title: format!(" Logs {}/{} — {} ", app.client_slug, app.slug, container),
        container,
        lines: Vec::new(),
        query: String::new(),
        filter_active: false,
        follow: true,
        paused: false,
        rx,
        child: Some(child),
    })
}

fn spawn_shell_modal(app: &HostingApp, area: Rect) -> Result<ShellModal> {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};

    let container = docker::container_name_from_upstream(&app.upstream)
        .ok_or_else(|| color_eyre::eyre::eyre!("app no apunta a contenedor local"))?;
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: area.height.saturating_sub(4).max(5),
            cols: area.width.saturating_sub(4).max(20),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| color_eyre::eyre::eyre!(e.to_string()))?;
    let mut cmd = CommandBuilder::new("docker");
    cmd.args(["exec", "-it", &container, "/bin/sh"]);
    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| color_eyre::eyre::eyre!(e.to_string()))?;
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| color_eyre::eyre::eyre!(e.to_string()))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| color_eyre::eyre::eyre!(e.to_string()))?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let chunk = String::from_utf8_lossy(&buf[..n]).to_string();
                    let _ = tx.send(chunk);
                }
            }
        }
    });
    Ok(ShellModal {
        title: format!(
            " Shell {}/{} — {}  Ctrl+Q: salir ",
            app.client_slug, app.slug, container
        ),
        output: Vec::new(),
        rx,
        writer,
        child,
    })
}

struct TerminalRestoreGuard {
    active: bool,
}

impl TerminalRestoreGuard {
    fn new() -> Self {
        Self { active: true }
    }

    fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for TerminalRestoreGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
        }
    }
}

// ── Main loop ─────────────────────────────────────────────────────────────────

pub fn run(store: &Store, cfg: &Config) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let mut restore_guard = TerminalRestoreGuard::new();
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut state = TuiState::load(store, cfg)?;

    let result = loop {
        let now = std::time::Instant::now();
        if now.duration_since(state.last_sys_refresh) >= std::time::Duration::from_secs(3) {
            state.sys_info = load_sys_info();
            state.last_sys_refresh = now;
        }
        state.poll_modal_streams();

        terminal.draw(|frame| draw(frame, &mut state))?;

        if event::poll(std::time::Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                // q/Esc solo sale si no hay modal ni filtro activo
                if (key.code == KeyCode::Char('q') || key.code == KeyCode::Esc)
                    && matches!(state.modal, Modal::None)
                    && !state.filter_active
                {
                    break Ok(());
                }
                let result = if state.filter_active && matches!(state.modal, Modal::None) {
                    handle_filter_key(&mut state, key.code);
                    Ok(())
                } else {
                    match &state.modal {
                        Modal::None => {
                            let (w, h) = crossterm::terminal::size()?;
                            handle_main_key(&mut state, key.code, store, cfg, Rect::new(0, 0, w, h))
                        }
                        Modal::Form { .. } => handle_form_key(&mut state, key.code, store, cfg),
                        Modal::Confirm { .. } => {
                            handle_confirm_key(&mut state, key.code, store, cfg)
                        }
                        Modal::Result { .. } => {
                            handle_result_key(&mut state, key.code);
                            Ok(())
                        }
                        Modal::Logs(_) => {
                            handle_logs_key(&mut state, key.code);
                            Ok(())
                        }
                        Modal::Shell(_) => handle_shell_key(&mut state, key),
                    }
                };
                if let Err(e) = result {
                    state.status = format!("error: {e}");
                }
            }
        }
    };

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    restore_guard.disarm();
    result
}

// ── Drawing ───────────────────────────────────────────────────────────────────

fn draw(frame: &mut Frame, state: &mut TuiState) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);
    draw_tabs(frame, state, chunks[0]);
    match state.tab {
        ActiveTab::Dashboard => draw_dashboard(frame, state, chunks[1]),
        ActiveTab::Clients => draw_clients(frame, state, chunks[1]),
        ActiveTab::Apps => draw_apps(frame, state, chunks[1]),
        ActiveTab::Databases => draw_databases(frame, state, chunks[1]),
        ActiveTab::Backups => draw_backups(frame, state, chunks[1]),
        ActiveTab::Ssh => draw_ssh(frame, state, chunks[1]),
    }
    draw_statusbar(frame, state, chunks[2]);
    draw_modal(frame, &mut state.modal, area);
}

fn draw_tabs(frame: &mut Frame, state: &TuiState, area: Rect) {
    let ok = state.doctor_checks.iter().filter(|c| c.ok).count();
    let fail = state.doctor_checks.iter().filter(|c| !c.ok).count();
    let d_label = if !state.doctor_loaded {
        "1 Dashboard".to_string()
    } else if fail > 0 {
        format!("1 Dashboard ✗{fail}")
    } else {
        format!("1 Dashboard ✓{ok}")
    };
    let titles: Vec<Line> = vec![
        Line::from(d_label),
        Line::from("2 Clients"),
        Line::from("3 Apps"),
        Line::from("4 Databases"),
        Line::from("5 Backups"),
        Line::from(format!("6 SSH ({})", state.ssh_users.len())),
    ];
    frame.render_widget(
        Tabs::new(titles)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Nubit Hosting Panel "),
            )
            .select(state.tab.index())
            .style(Style::default().fg(Color::White))
            .highlight_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        area,
    );
}

fn draw_statusbar(frame: &mut Frame, state: &TuiState, area: Rect) {
    let (msg, style) = if state.filter_active {
        (
            format!(
                "  / filtrar: {}▌   Enter: confirmar   Esc: limpiar",
                state.filter_query
            ),
            Style::default().fg(Color::Cyan),
        )
    } else if !state.filter_query.is_empty() {
        (
            format!(
                "  Filtro activo: '{}'   / para cambiar   Esc para limpiar   {}",
                state.filter_query,
                match state.tab {
                    ActiveTab::Clients => "↑↓: nav   a/e/d   q: quit",
                    ActiveTab::Apps => "↑↓: nav   a/e/d   Tab: aliases   q: quit",
                    _ => "↑↓: nav   q: quit",
                }
            ),
            Style::default().fg(Color::Yellow),
        )
    } else if !state.status.is_empty() {
        (state.status.clone(), Style::default().fg(Color::DarkGray))
    } else {
        let hint = match state.tab {
            ActiveTab::Clients => {
                "  1-6: tab   ↑↓: nav   a: add   e: edit   d: delete   /: filtrar   r: refresh   q: quit"
            }
            ActiveTab::Apps => match state.app_focus {
                AppFocus::Apps => {
                    "  Tab: aliases   ↑↓: nav   a/e/d   l: logs   s: shell   /: filtrar   c: caddy   r: refresh   q: quit"
                }
                AppFocus::Aliases => {
                    "  Tab: apps   ↑↓: nav   a: add alias   d: delete alias   c: caddy   r: refresh   q: quit"
                }
            },
            ActiveTab::Databases => match state.db_focus {
                DbFocus::Servers => {
                    "  Tab: foco grants   ↑↓: nav   a: add server   p: provisionar   r: refresh   q: quit"
                }
                DbFocus::Grants => {
                    "  Tab: foco servers   ↑↓: nav   e: reset password   m: mover/reasignar   r: refresh   q: quit"
                }
            },
            ActiveTab::Backups => {
                "  1-6: tab   ↑↓: nav   a: nuevo backup   Enter: restaurar   r: refresh   q: quit"
            }
            ActiveTab::Dashboard => "  1-6: tab   r: refresh   q: quit",
            ActiveTab::Ssh => match state.ssh_focus {
                SshFocus::Users => {
                    "  Tab: claves   ↑↓: nav   a: add user   e: edit   d: delete   /: filtrar   r: refresh   q: quit"
                }
                SshFocus::Keys => {
                    "  Tab: users   ↑↓: nav   a: add clave   d: delete clave   r: refresh   q: quit"
                }
            },
        };
        (hint.to_string(), Style::default().fg(Color::DarkGray))
    };
    frame.render_widget(Paragraph::new(msg).style(style), area);
}

fn draw_dashboard(frame: &mut Frame, state: &TuiState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(area);

    let si = &state.sys_info;
    let (cpu_bar, cpu_color) = pct_bar(si.cpu_usage_pct as u64, 100, 14);
    let (ram_bar, ram_color) = pct_bar(si.ram_used, si.ram_total, 14);
    let (disk_bar, disk_color) = pct_bar(si.disk_used, si.disk_total, 14);

    let mut left: Vec<Line> = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  Host:  ", Style::default().fg(Color::Gray)),
            Span::styled(
                si.hostname.clone(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("  IP:    ", Style::default().fg(Color::Gray)),
            Span::styled(si.primary_ip.clone(), Style::default().fg(Color::Cyan)),
        ]),
        Line::from(vec![
            Span::styled("  Load:  ", Style::default().fg(Color::Gray)),
            Span::styled(si.load_avg.clone(), Style::default().fg(Color::White)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  CPU:   ", Style::default().fg(Color::Gray)),
            Span::styled(cpu_bar, Style::default().fg(cpu_color)),
        ]),
        Line::from(vec![
            Span::styled("  RAM:   ", Style::default().fg(Color::Gray)),
            Span::styled(ram_bar, Style::default().fg(ram_color)),
            Span::styled(
                format!(" {}/{}", fmt_bytes(si.ram_used), fmt_bytes(si.ram_total)),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Disk:  ", Style::default().fg(Color::Gray)),
            Span::styled(disk_bar, Style::default().fg(disk_color)),
            Span::styled(
                format!(" {}/{}", fmt_bytes(si.disk_used), fmt_bytes(si.disk_total)),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(""),
        Line::from(vec![Span::styled(
            "  Panel:",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Clients:    ", Style::default().fg(Color::Gray)),
            Span::styled(
                state.clients.len().to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Apps:       ", Style::default().fg(Color::Gray)),
            Span::styled(
                state.apps.len().to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("  DB Servers: ", Style::default().fg(Color::Gray)),
            Span::styled(
                state.db_servers.len().to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Backups:    ", Style::default().fg(Color::Gray)),
            Span::styled(
                state.backups.len().to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
    ];
    if !state.db_servers.is_empty() {
        left.push(Line::from(""));
        for s in &state.db_servers {
            left.push(Line::from(format!(
                "    {} ({}) {}:{}",
                s.name, s.kind, s.host, s.port
            )));
        }
    }
    frame.render_widget(
        Paragraph::new(left).block(Block::default().borders(Borders::ALL).title(" Sistema ")),
        chunks[0],
    );

    let right_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(10), Constraint::Min(0)])
        .split(chunks[1]);

    let container_apps: Vec<(&HostingApp, String)> = state
        .apps
        .iter()
        .filter_map(|app| {
            docker::container_name_from_upstream(&app.upstream).map(|name| (app, name))
        })
        .collect();
    let running = container_apps
        .iter()
        .filter(|(app, _)| {
            state
                .container_status
                .get(&app.id)
                .is_some_and(|status| status == "running")
        })
        .count();
    let known_down = container_apps
        .iter()
        .filter(|(app, _)| {
            state
                .container_status
                .get(&app.id)
                .is_some_and(|status| status != "running")
        })
        .count();
    let unknown = container_apps.len().saturating_sub(running + known_down);
    let mut container_lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  Running: ", Style::default().fg(Color::Gray)),
            Span::styled(
                running.to_string(),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  Down: ", Style::default().fg(Color::Gray)),
            Span::styled(
                known_down.to_string(),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  Unknown: ", Style::default().fg(Color::Gray)),
            Span::styled(
                unknown.to_string(),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
    ];
    for (app, container) in container_apps.iter().take(5) {
        let status = state
            .container_status
            .get(&app.id)
            .map(String::as_str)
            .unwrap_or("unknown");
        let (marker, color) = match status {
            "running" => ("●", Color::Green),
            "unknown" => ("?", Color::Yellow),
            _ => ("●", Color::Red),
        };
        container_lines.push(Line::from(vec![
            Span::styled(format!("  {marker} "), Style::default().fg(color)),
            Span::styled(
                format!("{}/{}", app.client_slug, app.slug),
                Style::default().fg(Color::White),
            ),
            Span::styled(
                format!(" — {container} ({status})"),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }
    if container_apps.len() > 5 {
        container_lines.push(Line::from(format!("  … {} más", container_apps.len() - 5)));
    }
    if container_apps.is_empty() {
        container_lines.push(Line::from("  Sin apps con upstream de contenedor local"));
    }
    frame.render_widget(
        Paragraph::new(container_lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Contenedores "),
        ),
        right_chunks[0],
    );

    let doctor_lines: Vec<Line> = if !state.doctor_loaded {
        vec![Line::from("  Ejecutando checks...")]
    } else {
        std::iter::once(Line::from(""))
            .chain(state.doctor_checks.iter().map(|c| {
                let (m, color) = if c.ok {
                    ("✓", Color::Green)
                } else {
                    ("✗", Color::Red)
                };
                Line::from(vec![
                    Span::styled(
                        format!("  {m} "),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(c.name.clone(), Style::default().fg(Color::White)),
                    Span::styled(
                        format!(" — {}", c.detail),
                        Style::default().fg(Color::DarkGray),
                    ),
                ])
            }))
            .collect()
    };
    let ok = state.doctor_checks.iter().filter(|c| c.ok).count();
    let fail = state.doctor_checks.iter().filter(|c| !c.ok).count();
    let dtitle = if state.doctor_loaded {
        format!(" Doctor — {ok} ok / {fail} fail ")
    } else {
        " Doctor ".to_string()
    };
    frame.render_widget(
        Paragraph::new(doctor_lines).block(Block::default().borders(Borders::ALL).title(dtitle)),
        right_chunks[1],
    );
}

fn draw_clients(frame: &mut Frame, state: &mut TuiState, area: Rect) {
    let clients = state.filtered_clients();
    let filter_indicator = if !state.filter_query.is_empty() {
        format!(" [{}/{}]", clients.len(), state.clients.len())
    } else {
        String::new()
    };
    let header = Row::new(vec!["Slug", "Name", "Email", "Notas", "Created"])
        .style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .height(1);
    let rows: Vec<Row> = clients
        .iter()
        .map(|c| {
            let notes_preview = c.notes.as_deref().unwrap_or("");
            let notes_display = if notes_preview.len() > 22 {
                format!("{}…", &notes_preview[..21])
            } else {
                notes_preview.to_string()
            };
            Row::new(vec![
                Cell::from(c.slug.clone()),
                Cell::from(c.name.clone()),
                Cell::from(c.email.clone().unwrap_or_default()),
                Cell::from(notes_display).style(Style::default().fg(Color::DarkGray)),
                Cell::from(c.created_at.format("%Y-%m-%d").to_string()),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Percentage(18),
            Constraint::Percentage(28),
            Constraint::Percentage(25),
            Constraint::Percentage(20),
            Constraint::Percentage(9),
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::ALL).title(format!(
        " Clients ({}){}  / para filtrar ",
        state.clients.len(),
        filter_indicator
    )))
    .row_highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_stateful_widget(table, area, &mut state.clients_table);
}

fn draw_apps(frame: &mut Frame, state: &mut TuiState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(area);

    let apps_focused = state.app_focus == AppFocus::Apps;
    let focused_style = Style::default().fg(Color::Cyan);
    let normal_style = Style::default().fg(Color::DarkGray);

    // ── Panel apps ────────────────────────────────────────────────
    let filtered_apps = state.filtered_apps_view();
    let filter_indicator = if !state.filter_query.is_empty() {
        format!(" [{}/{}]", filtered_apps.len(), state.apps.len())
    } else {
        String::new()
    };
    let header = Row::new(vec!["", "Client", "App", "Domain", "Upstream", "Created"])
        .style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .height(1);
    let rows: Vec<Row> = filtered_apps
        .iter()
        .map(|a| {
            let alias_count = state.aliases.iter().filter(|al| al.app_id == a.id).count();
            let domain_cell = if alias_count > 0 {
                format!("{} +{}", a.domain, alias_count)
            } else {
                a.domain.clone()
            };
            let (status_sym, status_color) =
                match state.container_status.get(&a.id).map(String::as_str) {
                    Some("running") => ("▲", Color::Green),
                    Some("exited") | Some("dead") => ("▼", Color::Red),
                    Some(_) => ("●", Color::Yellow),
                    None => (" ", Color::DarkGray),
                };
            Row::new(vec![
                Cell::from(status_sym).style(Style::default().fg(status_color)),
                Cell::from(a.client_slug.clone()),
                Cell::from(a.slug.clone()),
                Cell::from(domain_cell),
                Cell::from(a.upstream.clone()),
                Cell::from(a.created_at.format("%Y-%m-%d").to_string()),
            ])
        })
        .collect();
    let apps_title = if apps_focused {
        format!(
            " Apps ({}){}  ● — a: add  e: edit  d: delete  Tab: aliases ",
            state.apps.len(),
            filter_indicator
        )
    } else {
        format!(
            " Apps ({}){}  — Tab: focus  /: filtrar ",
            state.apps.len(),
            filter_indicator
        )
    };
    let table = Table::new(
        rows,
        [
            Constraint::Percentage(3),
            Constraint::Percentage(13),
            Constraint::Percentage(11),
            Constraint::Percentage(33),
            Constraint::Percentage(27),
            Constraint::Percentage(10),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(apps_title)
            .border_style(if apps_focused {
                focused_style
            } else {
                normal_style
            }),
    )
    .row_highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_stateful_widget(table, chunks[0], &mut state.apps_table);

    // ── Panel aliases ──────────────────────────────────────────────
    let filtered_aliases: Vec<&DomainAlias> = state.filtered_aliases();
    let aliases_focused = state.app_focus == AppFocus::Aliases;
    let selected_app_name = state
        .selected_app()
        .map(|a| format!("{}/{}", a.client_slug, a.slug));

    let aliases_title = match (&selected_app_name, aliases_focused) {
        (Some(name), true) => format!(
            " Dominios Alias ● {} ({}) — a: add  d: delete ",
            name,
            filtered_aliases.len()
        ),
        (Some(name), false) => format!(
            " Dominios Alias {} ({}) — Tab: focus ",
            name,
            filtered_aliases.len()
        ),
        (None, _) => " Dominios Alias — selecciona una app ↑↓ ".to_string(),
    };

    let alias_header = Row::new(vec!["Dominio Alias", "Agregado"])
        .style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .height(1);
    let alias_rows: Vec<Row> = filtered_aliases
        .iter()
        .map(|al| {
            Row::new(vec![
                Cell::from(al.domain.clone()),
                Cell::from(al.created_at.format("%Y-%m-%d").to_string()),
            ])
        })
        .collect();
    let aliases_table = Table::new(
        alias_rows,
        [Constraint::Percentage(82), Constraint::Percentage(18)],
    )
    .header(alias_header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(aliases_title)
            .border_style(if aliases_focused {
                focused_style
            } else {
                normal_style
            }),
    )
    .row_highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_stateful_widget(aliases_table, chunks[1], &mut state.aliases_table);
}

fn draw_databases(frame: &mut Frame, state: &mut TuiState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(area);

    let servers_focused = state.db_focus == DbFocus::Servers;
    let focused_style = Style::default().fg(Color::Cyan);
    let normal_style = Style::default().fg(Color::DarkGray);

    let server_header = Row::new(vec!["Name", "Kind", "Host", "Port"])
        .style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .height(1);
    let server_rows: Vec<Row> = state
        .db_servers
        .iter()
        .map(|s| {
            Row::new(vec![
                Cell::from(s.name.clone()),
                Cell::from(s.kind.clone()),
                Cell::from(s.host.clone()),
                Cell::from(s.port.to_string()),
            ])
        })
        .collect();
    let servers_title = if servers_focused {
        " DB Servers ● — a: add  p: provision "
    } else {
        " DB Servers — Tab: focus "
    };
    let servers_table = Table::new(
        server_rows,
        [
            Constraint::Percentage(30),
            Constraint::Percentage(20),
            Constraint::Percentage(35),
            Constraint::Percentage(15),
        ],
    )
    .header(server_header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(servers_title)
            .border_style(if servers_focused {
                focused_style
            } else {
                normal_style
            }),
    )
    .row_highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_stateful_widget(servers_table, chunks[0], &mut state.db_table);

    let filtered_grants: Vec<&DatabaseGrant> = state.filtered_grants();
    let grants_focused = state.db_focus == DbFocus::Grants;
    let selected_server_name = state
        .db_table
        .selected()
        .and_then(|i| state.db_servers.get(i))
        .map(|s| s.name.clone());
    let grants_title_str = match (&selected_server_name, grants_focused) {
        (Some(name), true) => format!(
            " Grants ● {} ({}) — e: reset pw ",
            name,
            filtered_grants.len()
        ),
        (Some(name), false) => {
            format!(" Grants {} ({}) — Tab: focus ", name, filtered_grants.len())
        }
        (None, _) => format!(" Grants ({}) ", filtered_grants.len()),
    };
    let grant_header = Row::new(vec!["Client", "App", "Env", "Database", "User", "Host"])
        .style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .height(1);
    let grant_rows: Vec<Row> = filtered_grants
        .iter()
        .map(|g| {
            Row::new(vec![
                Cell::from(g.client_slug.clone()),
                Cell::from(g.app_slug.clone().unwrap_or_default()),
                Cell::from(g.env.clone()),
                Cell::from(g.db_name.clone()),
                Cell::from(g.username.clone()),
                Cell::from(g.host.clone()),
            ])
        })
        .collect();
    let grants_table = Table::new(
        grant_rows,
        [
            Constraint::Percentage(15),
            Constraint::Percentage(15),
            Constraint::Percentage(10),
            Constraint::Percentage(25),
            Constraint::Percentage(20),
            Constraint::Percentage(15),
        ],
    )
    .header(grant_header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(grants_title_str)
            .border_style(if grants_focused {
                focused_style
            } else {
                normal_style
            }),
    )
    .row_highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_stateful_widget(grants_table, chunks[1], &mut state.grants_table);
}

fn draw_backups(frame: &mut Frame, state: &mut TuiState, area: Rect) {
    let header = Row::new(vec!["Server", "Database", "File", "Size"])
        .style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .height(1);
    let rows: Vec<Row> = state
        .backups
        .iter()
        .map(|b| {
            Row::new(vec![
                Cell::from(b.server.clone()),
                Cell::from(b.database.clone()),
                Cell::from(b.filename.clone()),
                Cell::from(format_size(b.size_bytes)),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Percentage(15),
            Constraint::Percentage(30),
            Constraint::Percentage(40),
            Constraint::Percentage(15),
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::ALL).title(format!(
        " Backups ({}) — a: nuevo   Enter: restaurar ",
        state.backups.len()
    )))
    .row_highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_stateful_widget(table, area, &mut state.backups_table);
}

fn draw_ssh(frame: &mut Frame, state: &mut TuiState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(area);

    let users_focused = state.ssh_focus == SshFocus::Users;
    let focused_style = Style::default().fg(Color::Cyan);
    let normal_style = Style::default().fg(Color::DarkGray);

    let user_header = Row::new(vec![
        "Cliente", "App", "Username", "Shell", "Home Dir", "Creado",
    ])
    .style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )
    .height(1);
    let user_rows: Vec<Row> = state
        .ssh_users
        .iter()
        .map(|u| {
            Row::new(vec![
                Cell::from(u.client_slug.clone()),
                Cell::from(u.app_slug.clone().unwrap_or_else(|| "—".to_string())),
                Cell::from(u.username.clone()),
                Cell::from(u.shell.clone()),
                Cell::from(u.home_dir.clone()),
                Cell::from(u.created_at.format("%Y-%m-%d").to_string()),
            ])
        })
        .collect();
    let users_title = if users_focused {
        format!(
            " SSH Users ● ({}) — a: add  e: edit  d: delete ",
            state.ssh_users.len()
        )
    } else {
        format!(" SSH Users ({}) — Tab: focus ", state.ssh_users.len())
    };
    let users_table = Table::new(
        user_rows,
        [
            Constraint::Percentage(12),
            Constraint::Percentage(12),
            Constraint::Percentage(16),
            Constraint::Percentage(20),
            Constraint::Percentage(30),
            Constraint::Percentage(10),
        ],
    )
    .header(user_header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(users_title)
            .border_style(if users_focused {
                focused_style
            } else {
                normal_style
            }),
    )
    .row_highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_stateful_widget(users_table, chunks[0], &mut state.ssh_users_table);

    let filtered_keys: Vec<&SshKey> = state.filtered_ssh_keys();
    let keys_focused = state.ssh_focus == SshFocus::Keys;
    let selected_user_name = state.selected_ssh_user().map(|u| u.username.clone());

    let keys_title = match (&selected_user_name, keys_focused) {
        (Some(name), true) => format!(
            " SSH Keys ● {} ({}) — a: add  d: delete ",
            name,
            filtered_keys.len()
        ),
        (Some(name), false) => {
            format!(" SSH Keys {} ({}) — Tab: focus ", name, filtered_keys.len())
        }
        (None, _) => " SSH Keys — selecciona un usuario ↑↓ ".to_string(),
    };

    let key_header = Row::new(vec!["Etiqueta", "Tipo", "Comentario", "Creada"])
        .style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .height(1);
    let key_rows: Vec<Row> = filtered_keys
        .iter()
        .map(|k| {
            let parts: Vec<&str> = k.public_key.split_whitespace().collect();
            let key_type = parts.first().copied().unwrap_or("?");
            let comment = parts.get(2).copied().unwrap_or("");
            Row::new(vec![
                Cell::from(k.label.clone()),
                Cell::from(key_type.to_string()),
                Cell::from(comment.to_string()),
                Cell::from(k.created_at.format("%Y-%m-%d").to_string()),
            ])
        })
        .collect();
    let keys_table = Table::new(
        key_rows,
        [
            Constraint::Percentage(20),
            Constraint::Percentage(20),
            Constraint::Percentage(45),
            Constraint::Percentage(15),
        ],
    )
    .header(key_header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(keys_title)
            .border_style(if keys_focused {
                focused_style
            } else {
                normal_style
            }),
    )
    .row_highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_stateful_widget(keys_table, chunks[1], &mut state.ssh_keys_table);
}

// ── Modal rendering ───────────────────────────────────────────────────────────

fn draw_modal(frame: &mut Frame, modal: &mut Modal, area: Rect) {
    match modal {
        Modal::None => {}
        Modal::Form {
            title,
            fields,
            focus,
            error,
            ..
        } => {
            let height = (fields.len() * 4 + 4 + usize::from(error.is_some())) as u16;
            let popup = centered_rect(62, height, area);
            frame.render_widget(Clear, popup);
            draw_form(frame, title, fields, *focus, error.as_deref(), popup);
        }
        Modal::Confirm { message, .. } => {
            let lines = message.lines().count() as u16;
            let popup = centered_rect(54, lines + 5, area);
            frame.render_widget(Clear, popup);
            draw_confirm(frame, message, popup);
        }
        Modal::Result { title, lines } => {
            let height = (lines.len() + 5) as u16;
            let popup = centered_rect(60, height, area);
            frame.render_widget(Clear, popup);
            draw_result(frame, title, lines, popup);
        }
        Modal::Logs(logs) => draw_logs_modal(frame, logs, area),
        Modal::Shell(shell) => draw_shell_modal(frame, shell, area),
    }
}

fn draw_logs_modal(frame: &mut Frame, logs: &LogsModal, area: Rect) {
    let popup = centered_rect(92, area.height.saturating_sub(4).max(10), area);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(logs.title.clone())
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(inner);
    let query = logs.query.to_lowercase();
    let visible: Vec<Line> = logs
        .lines
        .iter()
        .filter(|line| query.is_empty() || line.to_lowercase().contains(&query))
        .rev()
        .take(chunks[0].height as usize)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|line| styled_log_line(line, &query))
        .collect();
    frame.render_widget(Paragraph::new(visible), chunks[0]);
    let hint = if logs.filter_active {
        format!(" /{}▌  Enter: aplicar  Esc: limpiar", logs.query)
    } else {
        format!(
            " /: buscar  p: pause={}  f: follow={}  c: clear  q/Esc: volver  container={}",
            logs.paused, logs.follow, logs.container
        )
    };
    frame.render_widget(
        Paragraph::new(hint).style(Style::default().fg(Color::DarkGray)),
        chunks[1],
    );
}

fn styled_log_line(line: &str, query: &str) -> Line<'static> {
    let lower = line.to_lowercase();
    let color = if lower.contains("error") || lower.contains("fatal") || lower.contains("panic") {
        Color::Red
    } else if lower.contains("warn") {
        Color::Yellow
    } else if lower.contains("info") {
        Color::Green
    } else {
        Color::White
    };
    let style = if !query.is_empty() && lower.contains(query) {
        Style::default().fg(color).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(color)
    };
    Line::from(Span::styled(line.to_string(), style))
}

fn draw_shell_modal(frame: &mut Frame, shell: &ShellModal, area: Rect) {
    let popup = centered_rect(96, area.height.saturating_sub(2).max(10), area);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(shell.title.clone())
        .border_style(Style::default().fg(Color::Green));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let lines: Vec<Line> = shell
        .output
        .iter()
        .rev()
        .take(inner.height as usize)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|line| {
            Line::from(Span::styled(
                line.clone(),
                Style::default().fg(Color::White),
            ))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_form(
    frame: &mut Frame,
    title: &str,
    fields: &[FormField],
    focus: usize,
    error: Option<&str>,
    area: Rect,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title.to_string())
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let has_selector = fields.get(focus).map(|f| f.is_selector()).unwrap_or(false);
    let hint = if has_selector {
        "  ←→/↑↓: seleccionar   Tab: sig. campo   Enter: confirmar   Esc: cancelar"
    } else {
        "  Tab: sig. campo   Shift+Tab: ant.   Enter: confirmar   Esc: cancelar"
    };

    let mut constraints: Vec<Constraint> = Vec::new();
    for _ in fields {
        constraints.push(Constraint::Length(1));
        constraints.push(Constraint::Length(3));
    }
    if error.is_some() {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Length(1));
    constraints.push(Constraint::Min(0));

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);
    let mut idx = 0;

    for (i, field) in fields.iter().enumerate() {
        let focused = i == focus;
        let req_mark = if field.required { " *" } else { "" };
        let label_style = if focused {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        frame.render_widget(
            Paragraph::new(format!(" {}{}", field.label, req_mark)).style(label_style),
            chunks[idx],
        );
        idx += 1;

        let (display, value_style, border_style) = if field.is_selector() && focused {
            let n = field.options.len();
            let cur = field
                .options
                .iter()
                .position(|o| o == &field.input.value)
                .unwrap_or(0);
            (
                format!(" ◀ {} ▶  ({}/{})", field.input.value, cur + 1, n),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
                Style::default().fg(Color::Cyan),
            )
        } else if field.is_selector() {
            (
                format!("  {}", field.input.value),
                Style::default().fg(Color::White),
                Style::default().fg(Color::DarkGray),
            )
        } else if field.input.value.is_empty() && !focused {
            (
                field.placeholder.to_string(),
                Style::default().fg(Color::DarkGray),
                Style::default().fg(Color::DarkGray),
            )
        } else {
            (
                format!(" {}{}", field.input.value, if focused { "_" } else { "" }),
                Style::default().fg(Color::White),
                if focused {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            )
        };

        frame.render_widget(
            Paragraph::new(display).style(value_style).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(border_style),
            ),
            chunks[idx],
        );
        idx += 1;
    }

    if let Some(err) = error {
        frame.render_widget(
            Paragraph::new(format!(" ⚠ {err}")).style(Style::default().fg(Color::Red)),
            chunks[idx],
        );
        idx += 1;
    }
    frame.render_widget(
        Paragraph::new(hint).style(Style::default().fg(Color::DarkGray)),
        chunks[idx],
    );
}

fn draw_confirm(frame: &mut Frame, message: &str, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Confirmar ")
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let lines = message.lines().count() as u16;
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(lines),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(inner);
    frame.render_widget(
        Paragraph::new(message.to_string()).style(Style::default().fg(Color::Yellow)),
        chunks[0],
    );
    frame.render_widget(
        Paragraph::new("  Enter: confirmar   Esc: cancelar")
            .style(Style::default().fg(Color::DarkGray)),
        chunks[2],
    );
}

fn draw_result(frame: &mut Frame, title: &str, lines: &[String], area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title.to_string())
        .border_style(Style::default().fg(Color::Green));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let lc = lines.len() as u16;
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(lc),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(inner);
    let content: Vec<Line> = lines
        .iter()
        .map(|l| {
            if l.starts_with("Password:") {
                Line::from(vec![
                    Span::styled("Password: ", Style::default().fg(Color::White)),
                    Span::styled(
                        l.trim_start_matches("Password: ").to_string(),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                ])
            } else {
                Line::from(l.clone())
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(content), chunks[0]);
    frame.render_widget(
        Paragraph::new("  Enter/Esc: cerrar").style(Style::default().fg(Color::DarkGray)),
        chunks[2],
    );
}

fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let w = (area.width * percent_x / 100).min(area.width);
    let x = area.x + area.width.saturating_sub(w) / 2;
    let h = height.min(area.height);
    let y = area.y + area.height.saturating_sub(h) / 2;
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}
