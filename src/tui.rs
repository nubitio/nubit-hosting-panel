use std::io;

use color_eyre::eyre::Result;
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    text::Line,
    widgets::{Block, Borders, List, ListItem, Paragraph},
};

use crate::store::Store;

pub fn run(store: &Store) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = loop {
        let clients = store.list_clients()?;
        let apps = store.list_apps()?;
        terminal.draw(|frame| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Percentage(50),
                    Constraint::Percentage(50),
                ])
                .split(frame.area());

            frame.render_widget(
                Paragraph::new("Nubit Hosting Panel — q para salir")
                    .style(Style::default().fg(Color::Cyan))
                    .block(Block::default().borders(Borders::ALL)),
                chunks[0],
            );

            let client_items: Vec<ListItem> = clients
                .iter()
                .map(|c| ListItem::new(Line::from(format!("{} — {}", c.slug, c.name))))
                .collect();
            frame.render_widget(
                List::new(client_items)
                    .block(Block::default().title("Clientes").borders(Borders::ALL)),
                chunks[1],
            );

            let app_items: Vec<ListItem> = apps
                .iter()
                .map(|a| {
                    ListItem::new(Line::from(format!(
                        "{}/{}  {} -> {}",
                        a.client_slug, a.slug, a.domain, a.upstream
                    )))
                })
                .collect();
            frame.render_widget(
                List::new(app_items).block(
                    Block::default()
                        .title("Sitios / Apps")
                        .borders(Borders::ALL),
                ),
                chunks[2],
            );
        })?;

        if event::poll(std::time::Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if key.code == KeyCode::Char('q') || key.code == KeyCode::Esc {
                    break Ok(());
                }
            }
        }
    };

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}
