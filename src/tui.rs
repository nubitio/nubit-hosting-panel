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
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState, Tabs},
};

use crate::store::{App as HostingApp, Client, DatabaseGrant, DbServer, Store};

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
        })
    }

    fn reload(&mut self, store: &Store) -> Result<()> {
        self.clients = store.list_clients()?;
        self.apps = store.list_apps()?;
        self.db_servers = store.list_db_servers()?;
        self.grants = store.list_database_grants()?;
        self.status = format!(
            "reloaded — clients:{} apps:{} db_servers:{} grants:{}",
            self.clients.len(),
            self.apps.len(),
            self.db_servers.len(),
            self.grants.len()
        );
        Ok(())
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

    fn switch_tab(&mut self, tab: ActiveTab) {
        self.tab = tab;
        self.status.clear();
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
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break Ok(()),
                    KeyCode::Tab => state.switch_tab(state.tab.next()),
                    KeyCode::BackTab => state.switch_tab(state.tab.prev()),
                    KeyCode::Char('1') => state.switch_tab(ActiveTab::Dashboard),
                    KeyCode::Char('2') => state.switch_tab(ActiveTab::Clients),
                    KeyCode::Char('3') => state.switch_tab(ActiveTab::Apps),
                    KeyCode::Char('4') => state.switch_tab(ActiveTab::Databases),
                    KeyCode::Down | KeyCode::Char('j') => state.nav_down(),
                    KeyCode::Up | KeyCode::Char('k') => state.nav_up(),
                    KeyCode::Char('r') => {
                        if let Err(err) = state.reload(store) {
                            state.status = format!("error: {err}");
                        }
                    }
                    _ => {}
                }
            }
        }
    };

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

// ── drawing ──────────────────────────────────────────────────────────────────

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

fn draw_dashboard(frame: &mut Frame, state: &TuiState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(area);

    let summary = vec![
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
    .collect::<Vec<_>>();

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
                .map(|name| g.server_name == name)
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

fn draw_statusbar(frame: &mut Frame, state: &TuiState, area: Rect) {
    let msg = if state.status.is_empty() {
        "  Tab/1-4: switch tab   ↑↓/j-k: navigate   r: refresh   q: quit".to_string()
    } else {
        format!("  {}", state.status)
    };
    frame.render_widget(
        Paragraph::new(msg).style(Style::default().fg(Color::DarkGray)),
        area,
    );
}
