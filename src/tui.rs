use std::{fs, io, path::PathBuf};

use color_eyre::eyre::Result;
use crossterm::{
    event::{self, Event, KeyCode},
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

use crate::{
    backup,
    config::Config,
    db, doctor,
    store::{App as HostingApp, Client, DatabaseGrant, DbServer, Store},
};

// ── System info ──────────────────────────────────────────────────────────────

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
        pct,
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
}

impl ActiveTab {
    fn index(self) -> usize {
        match self {
            Self::Dashboard => 0,
            Self::Clients => 1,
            Self::Apps => 2,
            Self::Databases => 3,
            Self::Backups => 4,
        }
    }
    fn from_index(i: usize) -> Self {
        match i {
            1 => Self::Clients,
            2 => Self::Apps,
            3 => Self::Databases,
            4 => Self::Backups,
            _ => Self::Dashboard,
        }
    }
    fn next(self) -> Self {
        Self::from_index((self.index() + 1) % 5)
    }
    fn prev(self) -> Self {
        Self::from_index((self.index() + 4) % 5)
    }
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
}

impl FormField {
    fn req(label: &'static str, placeholder: &'static str) -> Self {
        Self {
            label,
            input: Input::default(),
            required: true,
            placeholder,
        }
    }
    fn opt(label: &'static str, placeholder: &'static str) -> Self {
        Self {
            label,
            input: Input::default(),
            required: false,
            placeholder,
        }
    }
    fn prefill(mut self, value: &str) -> Self {
        self.input.value = value.to_string();
        self
    }
}

#[derive(Clone, Copy)]
enum FormKind {
    AddClient,
    AddApp,
    Provision,
    Backup,
}

#[derive(Clone, Copy)]
enum ConfirmKind {
    DeleteClient,
    DeleteApp,
}

enum Modal {
    None,
    Form {
        title: &'static str,
        fields: Vec<FormField>,
        focus: usize,
        kind: FormKind,
        error: Option<String>,
    },
    Confirm {
        message: String,
        kind: ConfirmKind,
        id: String,
    },
    Result {
        title: String,
        lines: Vec<String>,
    },
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

fn scan_backups(backup_dir: &std::path::Path) -> Vec<BackupEntry> {
    backup::list_backups(backup_dir, None, None)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|path| {
            let meta = fs::metadata(&path).ok()?;
            let filename = path.file_name()?.to_str()?.to_string();
            let rel = path.strip_prefix(backup_dir).ok()?;
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
    db_servers: Vec<DbServer>,
    grants: Vec<DatabaseGrant>,
    backups: Vec<BackupEntry>,
    doctor_checks: Vec<doctor::Check>,
    doctor_loaded: bool,
    sys_info: SysInfo,
    clients_table: TableState,
    apps_table: TableState,
    db_table: TableState,
    backups_table: TableState,
    status: String,
    modal: Modal,
}

impl TuiState {
    fn load(store: &Store, cfg: &Config) -> Result<Self> {
        let doctor_checks = doctor::run(cfg, store).unwrap_or_default();
        Ok(Self {
            tab: ActiveTab::Dashboard,
            clients: store.list_clients()?,
            apps: store.list_apps()?,
            db_servers: store.list_db_servers()?,
            grants: store.list_database_grants()?,
            backups: scan_backups(&cfg.backup_dir),
            doctor_checks,
            doctor_loaded: true,
            sys_info: load_sys_info(),
            clients_table: TableState::default(),
            apps_table: TableState::default(),
            db_table: TableState::default(),
            backups_table: TableState::default(),
            status: String::new(),
            modal: Modal::None,
        })
    }

    fn reload(&mut self, store: &Store, cfg: &Config) -> Result<()> {
        self.clients = store.list_clients()?;
        self.apps = store.list_apps()?;
        self.db_servers = store.list_db_servers()?;
        self.grants = store.list_database_grants()?;
        self.backups = scan_backups(&cfg.backup_dir);
        self.doctor_checks = doctor::run(cfg, store).unwrap_or_default();
        self.doctor_loaded = true;
        self.sys_info = load_sys_info();
        Ok(())
    }

    fn switch_tab(&mut self, tab: ActiveTab) {
        self.tab = tab;
        self.status.clear();
    }

    fn nav_down(&mut self) {
        match self.tab {
            ActiveTab::Clients => nav_down(&mut self.clients_table, self.clients.len()),
            ActiveTab::Apps => nav_down(&mut self.apps_table, self.apps.len()),
            ActiveTab::Databases => nav_down(&mut self.db_table, self.db_servers.len()),
            ActiveTab::Backups => nav_down(&mut self.backups_table, self.backups.len()),
            ActiveTab::Dashboard => {}
        }
    }

    fn nav_up(&mut self) {
        match self.tab {
            ActiveTab::Clients => nav_up(&mut self.clients_table, self.clients.len()),
            ActiveTab::Apps => nav_up(&mut self.apps_table, self.apps.len()),
            ActiveTab::Databases => nav_up(&mut self.db_table, self.db_servers.len()),
            ActiveTab::Backups => nav_up(&mut self.backups_table, self.backups.len()),
            ActiveTab::Dashboard => {}
        }
    }

    fn open_add(&mut self) {
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
                };
            }
            ActiveTab::Apps => {
                let prefill = self
                    .clients_table
                    .selected()
                    .and_then(|i| self.clients.get(i))
                    .map(|c| c.slug.as_str())
                    .unwrap_or("");
                self.modal = Modal::Form {
                    title: " Agregar App ",
                    fields: vec![
                        FormField::req("Cliente (slug)", "ej: acme-corp").prefill(prefill),
                        FormField::req("App slug", "ej: web"),
                        FormField::req("Dominio", "ej: acme.nubit.site"),
                        FormField::req("Upstream", "ej: container:8080"),
                    ],
                    focus: 0,
                    kind: FormKind::AddApp,
                    error: None,
                };
            }
            ActiveTab::Databases => {
                let prefill = self
                    .db_table
                    .selected()
                    .and_then(|i| self.db_servers.get(i))
                    .map(|s| s.name.as_str())
                    .unwrap_or("");
                self.modal = Modal::Form {
                    title: " Provisionar DB ",
                    fields: vec![
                        FormField::req("DB Server", "ej: mariadb").prefill(prefill),
                        FormField::req("Cliente (slug)", "ej: acme-corp"),
                        FormField::req("App (slug)", "ej: web"),
                        FormField::req("Env", "prod").prefill("prod"),
                        FormField::opt("Password", "vacío = auto-generar"),
                    ],
                    focus: 0,
                    kind: FormKind::Provision,
                    error: None,
                };
            }
            ActiveTab::Backups => {
                let server_prefill = self
                    .backups_table
                    .selected()
                    .and_then(|i| self.backups.get(i))
                    .map(|b| b.server.as_str())
                    .unwrap_or("");
                self.modal = Modal::Form {
                    title: " Nuevo Backup ",
                    fields: vec![
                        FormField::req("DB Server", "ej: mariadb").prefill(server_prefill),
                        FormField::req("Database", "ej: acme_web_prod"),
                    ],
                    focus: 0,
                    kind: FormKind::Backup,
                    error: None,
                };
            }
            _ => {}
        }
    }

    fn open_delete(&mut self) {
        match self.tab {
            ActiveTab::Clients => {
                if let Some(client) = self
                    .clients_table
                    .selected()
                    .and_then(|i| self.clients.get(i))
                {
                    self.modal = Modal::Confirm {
                        message: format!(
                            "Eliminar cliente '{}'?\nSe eliminarán también todas sus apps.",
                            client.slug
                        ),
                        kind: ConfirmKind::DeleteClient,
                        id: client.id.clone(),
                    };
                }
            }
            ActiveTab::Apps => {
                if let Some(app) = self.apps_table.selected().and_then(|i| self.apps.get(i)) {
                    self.modal = Modal::Confirm {
                        message: format!("Eliminar app '{}/{}'?", app.client_slug, app.slug),
                        kind: ConfirmKind::DeleteApp,
                        id: app.id.clone(),
                    };
                }
            }
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

fn handle_main_key(state: &mut TuiState, code: KeyCode, store: &Store, cfg: &Config) -> Result<()> {
    match code {
        KeyCode::Tab => state.switch_tab(state.tab.next()),
        KeyCode::BackTab => state.switch_tab(state.tab.prev()),
        KeyCode::Char('1') => state.switch_tab(ActiveTab::Dashboard),
        KeyCode::Char('2') => state.switch_tab(ActiveTab::Clients),
        KeyCode::Char('3') => state.switch_tab(ActiveTab::Apps),
        KeyCode::Char('4') => state.switch_tab(ActiveTab::Databases),
        KeyCode::Char('5') => state.switch_tab(ActiveTab::Backups),
        KeyCode::Down | KeyCode::Char('j') => state.nav_down(),
        KeyCode::Up | KeyCode::Char('k') => state.nav_up(),
        KeyCode::Char('a') => state.open_add(),
        KeyCode::Char('d') => state.open_delete(),
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
        _ => {}
    }
    Ok(())
}

fn handle_form_key(state: &mut TuiState, code: KeyCode, store: &Store, cfg: &Config) -> Result<()> {
    match code {
        KeyCode::Esc => state.modal = Modal::None,
        KeyCode::Tab => {
            if let Modal::Form { focus, fields, .. } = &mut state.modal {
                *focus = (*focus + 1) % fields.len();
            }
        }
        KeyCode::BackTab => {
            if let Modal::Form { focus, fields, .. } = &mut state.modal {
                *focus = (*focus + fields.len() - 1) % fields.len();
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
                fields[*focus].input.push(c);
                *error = None;
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
                fields[*focus].input.pop();
                *error = None;
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
        KeyCode::Enter => try_delete(state, store, cfg)?,
        _ => {}
    }
    Ok(())
}

fn handle_result_key(state: &mut TuiState, code: KeyCode) {
    if matches!(code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q')) {
        state.modal = Modal::None;
    }
}

fn try_submit(state: &mut TuiState, store: &Store, cfg: &Config) -> Result<()> {
    let (kind, data, first_empty) = match &state.modal {
        Modal::Form { kind, fields, .. } => {
            let data: Vec<String> = fields
                .iter()
                .map(|f| f.input.value.trim().to_string())
                .collect();
            let first_empty = fields
                .iter()
                .enumerate()
                .find(|(i, f)| f.required && data[*i].is_empty())
                .map(|(i, f)| (i, f.label));
            (*kind, data, first_empty)
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
                Ok(client) => {
                    state.modal = Modal::None;
                    state.reload(store, cfg)?;
                    state.status = format!("✓ cliente '{}' creado", client.slug);
                    state.switch_tab(ActiveTab::Clients);
                }
                Err(e) => set_modal_error(&mut state.modal, e.to_string()),
            }
        }
        FormKind::AddApp => {
            let client = data[0].clone();
            let slug = data[1].clone();
            let domain = data[2].clone();
            let upstream = data[3].clone();
            match store.add_app(&client, &slug, &domain, &upstream) {
                Ok(app) => {
                    state.modal = Modal::None;
                    state.reload(store, cfg)?;
                    state.status = format!("✓ app '{}/{}' creada", app.client_slug, app.slug);
                    state.switch_tab(ActiveTab::Apps);
                }
                Err(e) => set_modal_error(&mut state.modal, e.to_string()),
            }
        }
        FormKind::Provision => {
            let server_name = data[0].clone();
            let client = data[1].clone();
            let app = data[2].clone();
            let env = data[3].clone();
            let password = if data[4].is_empty() {
                rand_password()
            } else {
                data[4].clone()
            };
            match store.db_server(&server_name) {
                Err(e) => set_modal_error(&mut state.modal, format!("server no encontrado: {e}")),
                Ok(server) => {
                    match db::provision(
                        cfg, store, &server, &client, &app, &env, "%", None, None, password,
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
                                    "Guarda el password, no se volverá a mostrar.".to_string(),
                                ],
                            };
                            state.switch_tab(ActiveTab::Databases);
                        }
                        Err(e) => set_modal_error(&mut state.modal, e.to_string()),
                    }
                }
            }
        }
        FormKind::Backup => {
            let server_name = data[0].clone();
            let database = data[1].clone();
            match store.db_server(&server_name) {
                Err(e) => set_modal_error(&mut state.modal, format!("server no encontrado: {e}")),
                Ok(server) => match backup::backup(cfg, &server, &database, &cfg.backup_dir) {
                    Ok(path) => {
                        state.modal = Modal::None;
                        state.reload(store, cfg)?;
                        state.status = format!("✓ backup: {}", path.display());
                        state.switch_tab(ActiveTab::Backups);
                    }
                    Err(e) => set_modal_error(&mut state.modal, e.to_string()),
                },
            }
        }
    }
    Ok(())
}

fn try_delete(state: &mut TuiState, store: &Store, cfg: &Config) -> Result<()> {
    let (kind, id) = match &state.modal {
        Modal::Confirm { kind, id, .. } => (*kind, id.clone()),
        _ => return Ok(()),
    };
    match kind {
        ConfirmKind::DeleteClient => {
            store.delete_client(&id)?;
            state.modal = Modal::None;
            state.reload(store, cfg)?;
            state.clients_table.select(None);
            state.status = "✓ cliente eliminado".to_string();
        }
        ConfirmKind::DeleteApp => {
            store.delete_app(&id)?;
            state.modal = Modal::None;
            state.reload(store, cfg)?;
            state.apps_table.select(None);
            state.status = "✓ app eliminada".to_string();
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

// ── Main loop ─────────────────────────────────────────────────────────────────

pub fn run(store: &Store, cfg: &Config) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut state = TuiState::load(store, cfg)?;

    let result = loop {
        terminal.draw(|frame| draw(frame, &mut state))?;

        if event::poll(std::time::Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if key.code == KeyCode::Char('q') || key.code == KeyCode::Esc {
                    if matches!(state.modal, Modal::None) {
                        break Ok(());
                    }
                }
                let result = match &state.modal {
                    Modal::None => handle_main_key(&mut state, key.code, store, cfg),
                    Modal::Form { .. } => handle_form_key(&mut state, key.code, store, cfg),
                    Modal::Confirm { .. } => handle_confirm_key(&mut state, key.code, store, cfg),
                    Modal::Result { .. } => {
                        handle_result_key(&mut state, key.code);
                        Ok(())
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
    }

    draw_statusbar(frame, state, chunks[2]);
    draw_modal(frame, &mut state.modal, area);
}

fn draw_tabs(frame: &mut Frame, state: &TuiState, area: Rect) {
    let ok = state.doctor_checks.iter().filter(|c| c.ok).count();
    let fail = state.doctor_checks.iter().filter(|c| !c.ok).count();
    let doctor_label = if !state.doctor_loaded {
        "1 Dashboard".to_string()
    } else if fail > 0 {
        format!("1 Dashboard ✗{fail}")
    } else {
        format!("1 Dashboard ✓{ok}")
    };

    let titles: Vec<Line> = vec![
        Line::from(doctor_label),
        Line::from("2 Clients"),
        Line::from("3 Apps"),
        Line::from("4 Databases"),
        Line::from("5 Backups"),
    ];
    let tabs = Tabs::new(titles)
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
        );
    frame.render_widget(tabs, area);
}

fn draw_statusbar(frame: &mut Frame, state: &TuiState, area: Rect) {
    let msg = if !state.status.is_empty() {
        state.status.clone()
    } else {
        match state.tab {
            ActiveTab::Clients | ActiveTab::Apps => {
                "  Tab/1-5: tab   ↑↓/j-k: nav   a: add   d: delete   r: refresh   q: quit"
                    .to_string()
            }
            ActiveTab::Databases => {
                "  Tab/1-5: tab   ↑↓/j-k: nav   a: provisionar   r: refresh   q: quit".to_string()
            }
            ActiveTab::Backups => {
                "  Tab/1-5: tab   ↑↓/j-k: nav   a: nuevo backup   r: refresh   q: quit".to_string()
            }
            ActiveTab::Dashboard => "  Tab/1-5: tab   r: refresh doctor   q: quit".to_string(),
        }
    };
    frame.render_widget(
        Paragraph::new(msg).style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

fn draw_dashboard(frame: &mut Frame, state: &TuiState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(area);

    // ── Left: system info + panel summary ────────────────────────────────────
    let si = &state.sys_info;
    let (cpu_bar, cpu_color) = pct_bar(si.cpu_usage_pct as u64, 100, 14);
    let (ram_bar, ram_color) = pct_bar(si.ram_used, si.ram_total, 14);
    let (disk_bar, disk_color) = pct_bar(si.disk_used, si.disk_total, 14);

    let mut left_lines: Vec<Line> = vec![
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
        left_lines.push(Line::from(""));
        for s in &state.db_servers {
            left_lines.push(Line::from(format!(
                "    {} ({}) {}:{}",
                s.name, s.kind, s.host, s.port
            )));
        }
    }

    frame.render_widget(
        Paragraph::new(left_lines).block(Block::default().borders(Borders::ALL).title(" Sistema ")),
        chunks[0],
    );

    // ── Right: doctor checks ──────────────────────────────────────────────────
    let doctor_lines: Vec<Line> = if !state.doctor_loaded {
        vec![Line::from("  Ejecutando checks...")]
    } else {
        std::iter::once(Line::from(""))
            .chain(state.doctor_checks.iter().map(|check| {
                let (marker, color) = if check.ok {
                    ("✓", Color::Green)
                } else {
                    ("✗", Color::Red)
                };
                Line::from(vec![
                    Span::styled(
                        format!("  {marker} "),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(check.name.clone(), Style::default().fg(Color::White)),
                    Span::styled(
                        format!(" — {}", check.detail),
                        Style::default().fg(Color::DarkGray),
                    ),
                ])
            }))
            .collect()
    };

    let ok = state.doctor_checks.iter().filter(|c| c.ok).count();
    let fail = state.doctor_checks.iter().filter(|c| !c.ok).count();
    let title = if state.doctor_loaded {
        format!(" Doctor — {ok} ok / {fail} fail ")
    } else {
        " Doctor ".to_string()
    };

    frame.render_widget(
        Paragraph::new(doctor_lines).block(Block::default().borders(Borders::ALL).title(title)),
        chunks[1],
    );
}

fn draw_clients(frame: &mut Frame, state: &mut TuiState, area: Rect) {
    let header = Row::new(vec!["Slug", "Name", "Email", "Created"])
        .style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .height(1);
    let rows: Vec<Row> = state
        .clients
        .iter()
        .map(|c| {
            Row::new(vec![
                Cell::from(c.slug.clone()),
                Cell::from(c.name.clone()),
                Cell::from(c.email.clone().unwrap_or_default()),
                Cell::from(c.created_at.format("%Y-%m-%d").to_string()),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Percentage(20),
            Constraint::Percentage(35),
            Constraint::Percentage(30),
            Constraint::Percentage(15),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" Clients ({}) ", state.clients.len())),
    )
    .row_highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_stateful_widget(table, area, &mut state.clients_table);
}

fn draw_apps(frame: &mut Frame, state: &mut TuiState, area: Rect) {
    let header = Row::new(vec!["Client", "App", "Domain", "Upstream", "Created"])
        .style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .height(1);
    let rows: Vec<Row> = state
        .apps
        .iter()
        .map(|a| {
            Row::new(vec![
                Cell::from(a.client_slug.clone()),
                Cell::from(a.slug.clone()),
                Cell::from(a.domain.clone()),
                Cell::from(a.upstream.clone()),
                Cell::from(a.created_at.format("%Y-%m-%d").to_string()),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Percentage(15),
            Constraint::Percentage(12),
            Constraint::Percentage(35),
            Constraint::Percentage(28),
            Constraint::Percentage(10),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" Apps ({}) ", state.apps.len())),
    )
    .row_highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_stateful_widget(table, area, &mut state.apps_table);
}

fn draw_databases(frame: &mut Frame, state: &mut TuiState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(area);

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
    .block(Block::default().borders(Borders::ALL).title(format!(
        " DB Servers ({}) — a: provisionar ",
        state.db_servers.len()
    )))
    .row_highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_stateful_widget(servers_table, chunks[0], &mut state.db_table);

    let selected_server = state
        .db_table
        .selected()
        .and_then(|i| state.db_servers.get(i))
        .map(|s| s.name.clone());
    let filtered_grants: Vec<&DatabaseGrant> = state
        .grants
        .iter()
        .filter(|g| {
            selected_server
                .as_deref()
                .map(|n| g.server_name == n)
                .unwrap_or(true)
        })
        .collect();
    let grants_title = match &selected_server {
        Some(name) => format!(" Grants — {} ({}) ", name, filtered_grants.len()),
        None => format!(" Grants ({}) ", filtered_grants.len()),
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
    .block(Block::default().borders(Borders::ALL).title(grants_title));
    frame.render_widget(grants_table, chunks[1]);
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
        " Backups ({}) — a: nuevo backup ",
        state.backups.len()
    )))
    .row_highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_stateful_widget(table, area, &mut state.backups_table);
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
    }
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
        let label = format!(" {}{}", field.label, if field.required { " *" } else { "" });
        let label_style = if focused {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        frame.render_widget(Paragraph::new(label).style(label_style), chunks[idx]);
        idx += 1;

        let (display, value_style) = if field.input.value.is_empty() && !focused {
            (
                field.placeholder.to_string(),
                Style::default().fg(Color::DarkGray),
            )
        } else {
            (
                format!("{}{}", field.input.value, if focused { "_" } else { "" }),
                Style::default().fg(Color::White),
            )
        };
        let border_style = if focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        frame.render_widget(
            Paragraph::new(format!(" {display}"))
                .style(value_style)
                .block(
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
        Paragraph::new("  Tab: siguiente   Shift+Tab: anterior   Enter: confirmar   Esc: cancelar")
            .style(Style::default().fg(Color::DarkGray)),
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

    let line_count = lines.len() as u16;
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(line_count),
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
