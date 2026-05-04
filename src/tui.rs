use std::io;

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

use crate::store::{App as HostingApp, Client, DatabaseGrant, DbServer, Store};

// ── Tab ───────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum ActiveTab {
    Dashboard,
    Clients,
    Apps,
    Databases,
}

impl ActiveTab {
    fn index(self) -> usize {
        match self {
            Self::Dashboard => 0,
            Self::Clients => 1,
            Self::Apps => 2,
            Self::Databases => 3,
        }
    }
    fn from_index(i: usize) -> Self {
        match i {
            1 => Self::Clients,
            2 => Self::Apps,
            3 => Self::Databases,
            _ => Self::Dashboard,
        }
    }
    fn next(self) -> Self {
        Self::from_index((self.index() + 1) % 4)
    }
    fn prev(self) -> Self {
        Self::from_index((self.index() + 3) % 4)
    }
}

// ── Input / Form ──────────────────────────────────────────────────────────────

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
}

// ── State ─────────────────────────────────────────────────────────────────────

struct TuiState {
    tab: ActiveTab,
    clients: Vec<Client>,
    apps: Vec<HostingApp>,
    db_servers: Vec<DbServer>,
    grants: Vec<DatabaseGrant>,
    clients_table: TableState,
    apps_table: TableState,
    db_table: TableState,
    status: String,
    modal: Modal,
}

impl TuiState {
    fn load(store: &Store) -> Result<Self> {
        Ok(Self {
            tab: ActiveTab::Dashboard,
            clients: store.list_clients()?,
            apps: store.list_apps()?,
            db_servers: store.list_db_servers()?,
            grants: store.list_database_grants()?,
            clients_table: TableState::default(),
            apps_table: TableState::default(),
            db_table: TableState::default(),
            status: String::new(),
            modal: Modal::None,
        })
    }

    fn reload(&mut self, store: &Store) -> Result<()> {
        self.clients = store.list_clients()?;
        self.apps = store.list_apps()?;
        self.db_servers = store.list_db_servers()?;
        self.grants = store.list_database_grants()?;
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
            ActiveTab::Dashboard => {}
        }
    }

    fn nav_up(&mut self) {
        match self.tab {
            ActiveTab::Clients => nav_up(&mut self.clients_table, self.clients.len()),
            ActiveTab::Apps => nav_up(&mut self.apps_table, self.apps.len()),
            ActiveTab::Databases => nav_up(&mut self.db_table, self.db_servers.len()),
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
                        FormField::req("Upstream", "ej: container_name:8080"),
                    ],
                    focus: 0,
                    kind: FormKind::AddApp,
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

fn handle_main_key(state: &mut TuiState, code: KeyCode, store: &Store) -> Result<()> {
    match code {
        KeyCode::Tab => state.switch_tab(state.tab.next()),
        KeyCode::BackTab => state.switch_tab(state.tab.prev()),
        KeyCode::Char('1') => state.switch_tab(ActiveTab::Dashboard),
        KeyCode::Char('2') => state.switch_tab(ActiveTab::Clients),
        KeyCode::Char('3') => state.switch_tab(ActiveTab::Apps),
        KeyCode::Char('4') => state.switch_tab(ActiveTab::Databases),
        KeyCode::Down | KeyCode::Char('j') => state.nav_down(),
        KeyCode::Up | KeyCode::Char('k') => state.nav_up(),
        KeyCode::Char('a') => state.open_add(),
        KeyCode::Char('d') => state.open_delete(),
        KeyCode::Char('r') => {
            state.reload(store)?;
            state.status = format!(
                "actualizado — clients:{} apps:{}",
                state.clients.len(),
                state.apps.len()
            );
        }
        _ => {}
    }
    Ok(())
}

fn handle_form_key(state: &mut TuiState, code: KeyCode, store: &Store) -> Result<()> {
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
        KeyCode::Enter => try_submit(state, store)?,
        _ => {}
    }
    Ok(())
}

fn handle_confirm_key(state: &mut TuiState, code: KeyCode, store: &Store) -> Result<()> {
    match code {
        KeyCode::Esc => state.modal = Modal::None,
        KeyCode::Enter => try_delete(state, store)?,
        _ => {}
    }
    Ok(())
}

fn try_submit(state: &mut TuiState, store: &Store) -> Result<()> {
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
                    state.reload(store)?;
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
                    state.reload(store)?;
                    state.status = format!("✓ app '{}/{}' creada", app.client_slug, app.slug);
                    state.switch_tab(ActiveTab::Apps);
                }
                Err(e) => set_modal_error(&mut state.modal, e.to_string()),
            }
        }
    }
    Ok(())
}

fn try_delete(state: &mut TuiState, store: &Store) -> Result<()> {
    let (kind, id) = match &state.modal {
        Modal::Confirm { kind, id, .. } => (*kind, id.clone()),
        _ => return Ok(()),
    };
    match kind {
        ConfirmKind::DeleteClient => {
            store.delete_client(&id)?;
            state.modal = Modal::None;
            state.reload(store)?;
            state.clients_table.select(None);
            state.status = "✓ cliente eliminado".to_string();
        }
        ConfirmKind::DeleteApp => {
            store.delete_app(&id)?;
            state.modal = Modal::None;
            state.reload(store)?;
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

// ── Main loop ─────────────────────────────────────────────────────────────────

pub fn run(store: &Store) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut state = TuiState::load(store)?;

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
                    Modal::None => handle_main_key(&mut state, key.code, store),
                    Modal::Form { .. } => handle_form_key(&mut state, key.code, store),
                    Modal::Confirm { .. } => handle_confirm_key(&mut state, key.code, store),
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
    }

    draw_statusbar(frame, state, chunks[2]);
    draw_modal(frame, &mut state.modal, area);
}

fn draw_tabs(frame: &mut Frame, state: &TuiState, area: Rect) {
    let titles: Vec<Line> = ["1 Dashboard", "2 Clients", "3 Apps", "4 Databases"]
        .iter()
        .map(|t| Line::from(*t))
        .collect();
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
                "  Tab/1-4: tab   ↑↓/j-k: nav   a: agregar   d: eliminar   r: refresh   q: salir"
                    .to_string()
            }
            _ => "  Tab/1-4: tab   ↑↓/j-k: nav   r: refresh   q: salir".to_string(),
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
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(area);

    let summary: Vec<Line> = vec![
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
            Span::styled("  DB Grants:  ", Style::default().fg(Color::Gray)),
            Span::styled(
                state.grants.len().to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
        Line::from(vec![Span::styled(
            "  DB Servers:",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )]),
        Line::from(""),
    ]
    .into_iter()
    .chain(
        state
            .db_servers
            .iter()
            .map(|s| Line::from(format!("    {} ({}) {}:{}", s.name, s.kind, s.host, s.port))),
    )
    .collect();

    frame.render_widget(
        Paragraph::new(summary).block(Block::default().borders(Borders::ALL).title(" Summary ")),
        chunks[0],
    );

    let app_lines: Vec<Line> = std::iter::once(Line::from(""))
        .chain(state.apps.iter().map(|a| {
            Line::from(vec![
                Span::styled(
                    format!("  {}/{}", a.client_slug, a.slug),
                    Style::default().fg(Color::Cyan),
                ),
                Span::raw(format!("  {}  ", a.domain)),
                Span::styled(
                    format!("→ {}", a.upstream),
                    Style::default().fg(Color::DarkGray),
                ),
            ])
        }))
        .collect();

    frame.render_widget(
        Paragraph::new(app_lines).block(Block::default().borders(Borders::ALL).title(" Apps ")),
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
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" DB Servers ({}) ", state.db_servers.len())),
    )
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
        constraints.push(Constraint::Length(1)); // label
        constraints.push(Constraint::Length(3)); // input
    }
    if error.is_some() {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Length(1)); // hint
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
