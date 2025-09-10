use std::io::Stdout;
use std::process::Command;

use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen, Clear, ClearType};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Table, Row, Cell, TableState};
use ratatui::widgets::Clear as WidgetClear;
use ratatui::Terminal;

use crate::config::Config;
use crate::server::{Server, ServerCollection};

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

pub struct TuiApp {
    config: Config,
    collection: ServerCollection,
    input: String,
    selected: usize,
    state: TableState,
    editing: Option<usize>,
    edit_alias: String,
    edit_username: String,
    edit_address: String,
    edit_port: String,
    current_field: usize,
    deleting: Option<usize>,
    confirm_yes: bool,
    adding: bool,
    add_alias: String,
    add_username: String,
    add_address: String,
    add_port: String,
    add_current_field: usize,
    add_confirm_stage: bool,
    add_choice: bool,
    error_message: String,
    show_help: bool,
    quick_connect_focused: bool,
}

impl TuiApp {
    pub fn new(config: Config, collection: ServerCollection) -> Self {
        let mut state = TableState::default();
        state.select(Some(0));
        Self {
            config,
            collection,
            input: String::new(),
            selected: 0,
            state,
            editing: None,
            edit_alias: String::new(),
            edit_username: String::new(),
            edit_address: String::new(),
            edit_port: String::new(),
            current_field: 0,
            deleting: None,
            confirm_yes: false,
            adding: false,
            add_alias: String::new(),
            add_username: String::new(),
            add_address: String::new(),
            add_port: String::new(),
            add_current_field: 0,
            add_confirm_stage: false,
            add_choice: false,
            error_message: String::new(),
            show_help: false,
            quick_connect_focused: false,
        }
    }

    pub fn run(&mut self, terminal: &mut Tui) -> Result<(), Box<dyn std::error::Error>> {
        loop {
            terminal.draw(|f| self.ui(f))?;

            if let Event::Key(key) = event::read()? && key.kind == KeyEventKind::Press {
                if self.editing.is_some() {
                        // Edit mode
                        match key.code {
                            KeyCode::Tab => {
                                self.current_field = (self.current_field + 1) % 4;
                            }
                            KeyCode::Enter => {
                                // Save
                                let port: u16 = match self.edit_port.parse() {
                                    Ok(p) if p >= 1 => p,
                                    _ => {
                                        self.error_message = "Port must be between 1 and 65535".to_string();
                                        continue;
                                    }
                                };
                                if let Some(idx) = self.editing
                                    && let Some(old_alias) = self.collection.hosts().keys().nth(idx) {
                                        let old_alias = old_alias.clone();
                                        let new_server = Server {
                                            id: None,
                                            alias: Some(self.edit_alias.clone()),
                                            username: self.edit_username.clone(),
                                            address: self.edit_address.clone(),
                                            port,
                                            last_connect: None,
                                        };
                                        self.collection.remove(&old_alias);
                                        self.collection.insert(&self.edit_alias, new_server);
                                        self.collection.save_to_storage(&self.config.server_file_path);
                                    }
                                self.editing = None;
                                self.error_message.clear();
                            }
                            KeyCode::Esc => {
                                self.editing = None;
                            }
                            KeyCode::Char(c) => {
                                match self.current_field {
                                    0 => self.edit_alias.push(c),
                                    1 => self.edit_username.push(c),
                                    2 => self.edit_address.push(c),
                                    3 => if c.is_ascii_digit() { self.edit_port.push(c) },
                                    _ => {}
                                }
                            }
                            KeyCode::Backspace => {
                                match self.current_field {
                                    0 => { self.edit_alias.pop(); }
                                    1 => { self.edit_username.pop(); }
                                    2 => { self.edit_address.pop(); }
                                    3 => { self.edit_port.pop(); }
                                    _ => {}
                                }
                            }
                            _ => {}
                        }
                    } else if self.deleting.is_some() {
                        // Delete confirmation
                        match key.code {
                            KeyCode::Left => self.confirm_yes = false,
                            KeyCode::Right => self.confirm_yes = true,
                            KeyCode::Enter => {
                                if self.confirm_yes
                                    && let Some(idx) = self.deleting
                                        && let Some(alias) = self.collection.hosts().keys().nth(idx) {
                                            self.collection.remove(&alias.clone());
                                            self.collection.save_to_storage(&self.config.server_file_path);
                                            // Update selection
                                            if self.collection.hosts().is_empty() {
                                                self.selected = 0;
                                            } else if self.selected >= self.collection.hosts().len() {
                                                self.selected = self.collection.hosts().len().saturating_sub(1);
                                            }
                                            self.state.select(Some(self.selected));
                                        }
                                self.deleting = None;
                            }
                            KeyCode::Esc => {
                                self.deleting = None;
                            }
                            _ => {}
                        }
                    } else if self.adding {
                        // Add mode
                        if self.add_confirm_stage {
                            // Add confirmation
                            match key.code {
                                KeyCode::Left => self.add_choice = false,
                                KeyCode::Right => self.add_choice = true,
                                KeyCode::Enter => {
                                    if self.add_choice {
                                        let port: u16 = match self.add_port.parse() {
                                            Ok(p) if p >= 1 => p,
                                            _ => {
                                                self.error_message = "Port must be between 1 and 65535".to_string();
                                                continue;
                                            }
                                        };
                                        let server = Server {
                                            id: None,
                                            alias: Some(self.add_alias.clone()),
                                            username: self.add_username.clone(),
                                            address: self.add_address.clone(),
                                            port,
                                            last_connect: None,
                                        };
                                        self.collection.insert(&self.add_alias, server);
                                        self.collection.save_to_storage(&self.config.server_file_path);
                                        // Update selection to new server
                                        if let Some(pos) = self.collection.hosts().keys().position(|k| k == &self.add_alias) {
                                            self.selected = pos;
                                            self.state.select(Some(self.selected));
                                        }
                                    }
                                    self.adding = false;
                                    self.add_confirm_stage = false;
                                    self.error_message.clear();
                                }
                                KeyCode::Esc => {
                                    self.adding = false;
                                    self.add_confirm_stage = false;
                                }
                                _ => {}
                            }
                        } else {
                            // Add input
                            match key.code {
                                KeyCode::Tab => {
                                    self.add_current_field = (self.add_current_field + 1) % 4;
                                }
                                KeyCode::Enter => {
                                    // Go to confirmation
                                    self.add_confirm_stage = true;
                                    self.add_choice = false;
                                }
                                KeyCode::Esc => {
                                    self.adding = false;
                                }
                                KeyCode::Char(c) => {
                                    match self.add_current_field {
                                        0 => self.add_alias.push(c),
                                        1 => self.add_username.push(c),
                                        2 => self.add_address.push(c),
                                        3 => if c.is_ascii_digit() { self.add_port.push(c) },
                                        _ => {}
                                    }
                                }
                                KeyCode::Backspace => {
                                    match self.add_current_field {
                                        0 => { self.add_alias.pop(); }
                                        1 => { self.add_username.pop(); }
                                        2 => { self.add_address.pop(); }
                                        3 => { self.add_port.pop(); }
                                        _ => {}
                                    }
                                }
                                _ => {}
                            }
                        }
                    } else {
                        // Normal mode
                        if self.show_help {
                            // Help dialog is open - only allow h or esc to close it
                            match key.code {
                                KeyCode::Char('h') | KeyCode::Char('H') | KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => {
                                    self.show_help = false;
                                }
                                _ => {
                                    // Ignore all other keys when help is open
                                }
                            }
                        } else {
                            // Normal operation when help is not open
                            // Handle Quick Connect focus toggle
                            if key.code == KeyCode::Char('f') && key.modifiers.contains(KeyModifiers::CONTROL) {
                                self.quick_connect_focused = !self.quick_connect_focused;
                                if !self.quick_connect_focused {
                                    self.input.clear();
                                }
                                continue;
                            }

                            // If Quick Connect is focused, only handle input-related keys
                            if self.quick_connect_focused {
                                match key.code {
                                    KeyCode::Enter => {
                                        if !self.input.is_empty() {
                                            // Try to connect to the server with the entered alias (Quick Connect)
                                            if let Some(alias) = self.collection.hosts().keys().find(|k| k == &&self.input) {
                                                self.connect(terminal, &alias.clone())?;
                                            } else {
                                                self.error_message = format!("Server '{}' not found", self.input);
                                            }
                                            self.input.clear();
                                            self.quick_connect_focused = false;
                                        }
                                    }
                                    KeyCode::Esc => {
                                        self.input.clear();
                                        self.quick_connect_focused = false;
                                    }
                                    KeyCode::Char(c) => {
                                        self.input.push(c);
                                    }
                                    KeyCode::Backspace => {
                                        self.input.pop();
                                    }
                                    _ => {} // Ignore all other keys when Quick Connect is focused
                                }
                            } else {
                                // Normal key handling when Quick Connect is not focused
                                match key.code {
                                    KeyCode::Char('q') => return Ok(()),
                                    KeyCode::Down => self.next(),
                                    KeyCode::Up => self.previous(),
                                    KeyCode::Delete => {
                                        self.deleting = Some(self.selected);
                                        self.confirm_yes = false;
                                    }
                                    KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                        self.deleting = Some(self.selected);
                                        self.confirm_yes = false;
                                    }
                                    KeyCode::Char('n') | KeyCode::Char('N') => {
                                        self.adding = true;
                                        self.add_alias.clear();
                                        self.add_username = "root".to_string();
                                        self.add_address.clear();
                                        self.add_port = "22".to_string();
                                        self.add_current_field = 0;
                                        self.add_confirm_stage = false;
                                    }
                                    KeyCode::Char('e') | KeyCode::Char('E') => {
                                        if let Some(alias) = self.collection.hosts().keys().nth(self.selected)
                                            && let Some(server) = self.collection.get(&alias.clone()) {
                                                self.editing = Some(self.selected);
                                                self.edit_alias = alias.clone();
                                                self.edit_username = server.username.clone();
                                                self.edit_address = server.address.clone();
                                                self.edit_port = server.port.to_string();
                                                self.current_field = 0;
                                            }
                                    }
                                    KeyCode::Enter => {
                                        if let Some(alias) = self.collection.hosts().keys().nth(self.selected) {
                                            // Connect to selected server in list
                                            self.connect(terminal, &alias.clone())?;
                                        }
                                    }
                                    KeyCode::Char('h') | KeyCode::Char('H') => {
                                        self.show_help = true;
                                    }
                                    KeyCode::Char(c) => {
                                        self.input.push(c);
                                        self.quick_connect_focused = true; // Auto-focus when typing
                                    }
                                    KeyCode::Backspace => {
                                        self.input.pop();
                                        if self.input.is_empty() {
                                            self.quick_connect_focused = false;
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            }
    }

    fn ui(&mut self, f: &mut ratatui::Frame) {
        let size = f.area();

        if self.editing.is_some() {
            // Edit mode UI
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Length(3),
                    Constraint::Length(3),
                    Constraint::Length(3),
                    Constraint::Length(3),
                    Constraint::Length(3),
                ])
                .split(size);

            let fields = ["Alias", "Username", "Address", "Port"];
            let values = [
                &self.edit_alias,
                &self.edit_username,
                &self.edit_address,
                &self.edit_port,
            ];

            for i in 0..4 {
                let mut block = Block::default().borders(Borders::ALL).title(fields[i]);
                if i == self.current_field {
                    block = block.border_style(Style::default().fg(Color::Yellow).bg(Color::Black));
                }
                let para = Paragraph::new(values[i].as_str()).block(block);
                f.render_widget(para, chunks[i]);
            }

            let help = Paragraph::new("Tab: Next Field | Enter: Save | Esc: Cancel")
                .style(Style::default().fg(Color::White))
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::ALL).title("Edit Mode"));
            f.render_widget(help, chunks[4]);

            // Error message
            if !self.error_message.is_empty() {
                let error = Paragraph::new(self.error_message.as_str())
                    .style(Style::default().fg(Color::Red))
                    .alignment(Alignment::Center)
                    .block(Block::default().borders(Borders::ALL).title("Error"));
                f.render_widget(error, chunks[5]);
            }
        } else if self.adding {
            // Add mode UI
            if self.add_confirm_stage {
                // Add confirmation dialog
                let area = centered_rect(60, 20, size);
                let block = Block::default()
                    .borders(Borders::ALL)
                    .title("Confirm Add Server")
                    .border_style(Style::default().fg(Color::Green));

                let mut text = vec![
                    Line::from("Add this server?"),
                    Line::from(format!("Alias: {}", self.add_alias)),
                    Line::from(format!("Username: {}", self.add_username)),
                    Line::from(format!("Address: {}", self.add_address)),
                    Line::from(format!("Port: {}", self.add_port)),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("No", if !self.add_choice { Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD) } else { Style::default() }),
                        Span::raw("  /  "),
                        Span::styled("Yes", if self.add_choice { Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD) } else { Style::default() }),
                    ]),
                    Line::from(""),
                    Line::from("←/→: Switch | Enter: Confirm | Esc: Cancel"),
                ];

                // Add error message if present
                if !self.error_message.is_empty() {
                    text.insert(0, Line::from(""));
                    text.insert(0, Line::from(vec![Span::styled(&self.error_message, Style::default().fg(Color::Red))]));
                }

                let para = Paragraph::new(text)
                    .block(block)
                    .alignment(Alignment::Center);

                f.render_widget(WidgetClear, area);
                f.render_widget(para, area);
            } else {
                // Add input UI
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(3),
                        Constraint::Length(3),
                        Constraint::Length(3),
                        Constraint::Length(3),
                        Constraint::Length(3),
                        Constraint::Length(3),
                    ])
                    .split(size);

                let fields = ["Alias", "Username", "Address", "Port"];
                let values = [
                    &self.add_alias,
                    &self.add_username,
                    &self.add_address,
                    &self.add_port,
                ];

                for i in 0..4 {
                    let mut block = Block::default().borders(Borders::ALL).title(fields[i]);
                    if i == self.add_current_field {
                        block = block.border_style(Style::default().fg(Color::Green));
                    }
                    let para = Paragraph::new(values[i].as_str()).block(block);
                    f.render_widget(para, chunks[i]);
                }

                let help = Paragraph::new("Tab: Next Field | Enter: Confirm | Esc: Cancel")
                    .style(Style::default().fg(Color::White))
                    .alignment(Alignment::Center)
                    .block(Block::default().borders(Borders::ALL).title("Add Server"));
                f.render_widget(help, chunks[4]);

                // Error message
                if !self.error_message.is_empty() {
                    let error = Paragraph::new(self.error_message.as_str())
                        .style(Style::default().fg(Color::Red))
                        .alignment(Alignment::Center)
                        .block(Block::default().borders(Borders::ALL).title("Error"));
                    f.render_widget(error, chunks[5]);
                }
            }
        } else {
            // Normal mode UI
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),  // Title
                    Constraint::Length(3),  // Input
                    Constraint::Min(5),     // Server list
                    Constraint::Length(4),  // Status & Help
                ])
                .split(size);

            // Title
            let title = Paragraph::new("🚀 PSM - SSH Manager")
                .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
                .alignment(Alignment::Center)
                .block(Block::default()
                    .borders(Borders::ALL)
                    .border_type(ratatui::widgets::BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Blue))
                    .title("Main Menu")
                    .title_style(Style::default().fg(Color::White).add_modifier(Modifier::BOLD)));
            f.render_widget(title, chunks[0]);

            // Input box with better styling
            let input_block = Block::default()
                .borders(Borders::ALL)
                .border_type(ratatui::widgets::BorderType::Rounded)
                .border_style(Style::default().fg(if self.quick_connect_focused { Color::Red } else { Color::Yellow }))
                .title(if self.quick_connect_focused { "🔍 Quick Connect (FOCUSED)" } else { "🔍 Quick Connect" })
                .title_style(Style::default().fg(if self.quick_connect_focused { Color::Red } else { Color::Yellow }).add_modifier(Modifier::BOLD));

            let input = Paragraph::new(self.input.as_str())
                .style(Style::default().fg(if self.quick_connect_focused { Color::Red } else { Color::Yellow }))
                .block(input_block);
            f.render_widget(input, chunks[1]);

            // Server table with enhanced styling
            let server_count = self.collection.hosts().len();
            let table_block = Block::default()
                .borders(Borders::ALL)
                .border_type(ratatui::widgets::BorderType::Rounded)
                .border_style(Style::default().fg(Color::Green))
                .title(format!("📋 Servers ({})", server_count))
                .title_style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD));

            // Table headers
            let header_cells = ["#", "Alias", "Username", "Address", "Port", "Last Connect"]
                .iter()
                .map(|h| Cell::from(*h));
            let header = Row::new(header_cells)
                .height(1);

            // Table rows
            let rows: Vec<Row> = self
                .collection
                .hosts()
                .iter()
                .enumerate()
                .map(|(index, (alias, server))| {
                    let is_selected = Some(index) == self.state.selected();
                    let base_style = if is_selected {
                        Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    };

                    let cells = vec![
                        Cell::from(format!("{:2}", index + 1))
                            .style(Style::default().fg(if is_selected { Color::Black } else { Color::Gray })),
                        Cell::from(format!("{:<15}", alias.chars().take(15).collect::<String>()))
                            .style(Style::default().fg(if is_selected { Color::Black } else { Color::Cyan }).add_modifier(if is_selected { Modifier::BOLD } else { Modifier::empty() })),
                        Cell::from(format!("{:<12}", server.username.chars().take(12).collect::<String>()))
                            .style(Style::default().fg(if is_selected { Color::Black } else { Color::Green })),
                        Cell::from(format!("{:<20}", server.address.chars().take(20).collect::<String>()))
                            .style(Style::default().fg(if is_selected { Color::Black } else { Color::Green })),
                        Cell::from(format!("{:>5}", server.port))
                            .style(Style::default().fg(if is_selected { Color::Black } else { Color::Magenta })),
                        Cell::from(format!("{:<19}", server.get_last_connect_display().chars().take(19).collect::<String>()))
                            .style(Style::default().fg(if is_selected { Color::Black } else { Color::Yellow })),
                    ];

                    Row::new(cells).style(base_style).height(1)
                })
                .collect();

            let table = Table::new(rows, [
                Constraint::Length(3),  // #
                Constraint::Length(16), // Alias
                Constraint::Length(13), // Username
                Constraint::Length(21), // Address
                Constraint::Length(6),  // Port
                Constraint::Min(20),    // Last Connect
            ])
            .header(header)
            .block(table_block)
            .row_highlight_style(Style::default().bg(Color::Cyan).fg(Color::Black).add_modifier(Modifier::BOLD))
            .highlight_symbol("▶ ");

            f.render_stateful_widget(table, chunks[2], &mut self.state);

            // Status and Help combined
            let status_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1),  // Status
                    Constraint::Length(2),  // Help
                ])
                .split(chunks[3]);

            // Status bar
            let selected_info = if let Some(idx) = self.state.selected() {
                if let Some((alias, _)) = self.collection.hosts().iter().nth(idx) {
                    format!("Selected: {}", alias)
                } else {
                    "No server selected".to_string()
                }
            } else {
                "No server selected".to_string()
            };

            let status = Paragraph::new(selected_info)
                .style(Style::default().fg(Color::White))
                .alignment(Alignment::Left)
                .block(Block::default().borders(Borders::TOP).border_style(Style::default().fg(Color::Gray)));
            f.render_widget(status, status_chunks[0]);

            // Help with better formatting
            let help_lines = vec![
                Line::from(vec![
                    Span::styled("Navigation: ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                    Span::styled("↑/↓", Style::default().fg(Color::Yellow)),
                    Span::styled(" | Connect: ", Style::default().fg(Color::Cyan)),
                    Span::styled("Enter", Style::default().fg(Color::Green)),
                    Span::styled(" | Quick Connect: ", Style::default().fg(Color::Cyan)),
                    Span::styled("Ctrl+F", Style::default().fg(Color::Red)),
                ]),
                Line::from(vec![
                    Span::styled("Actions: ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                    Span::styled("e", Style::default().fg(Color::Blue)),
                    Span::styled("-Edit | ", Style::default().fg(Color::Gray)),
                    Span::styled("n", Style::default().fg(Color::Green)),
                    Span::styled("-Add | ", Style::default().fg(Color::Gray)),
                    Span::styled("Del", Style::default().fg(Color::Red)),
                    Span::styled("-Delete | ", Style::default().fg(Color::Gray)),
                    Span::styled("h", Style::default().fg(Color::Magenta)),
                    Span::styled("-Help | ", Style::default().fg(Color::Gray)),
                    Span::styled("q", Style::default().fg(Color::Magenta)),
                    Span::styled("-Quit", Style::default().fg(Color::Gray)),
                ]),
            ];

            let help = Paragraph::new(help_lines)
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::TOP).border_style(Style::default().fg(Color::Gray)));
            f.render_widget(help, status_chunks[1]);

            // Delete confirmation dialog with better styling
            if self.deleting.is_some() {
                let area = centered_rect(70, 25, size);
                let block = Block::default()
                    .borders(Borders::ALL)
                    .border_type(ratatui::widgets::BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Red))
                    .title("⚠️  Confirm Delete")
                    .title_style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD));

                let text = vec![
                    Line::from(""),
                    Line::from("🗑️  Are you sure you want to delete this server?"),
                    Line::from(""),
                    Line::from("This action cannot be undone."),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("❌ No", if !self.confirm_yes {
                            Style::default().fg(Color::White).bg(Color::Red).add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(Color::Gray)
                        }),
                        Span::raw("     "),
                        Span::styled("✅ Yes", if self.confirm_yes {
                            Style::default().fg(Color::White).bg(Color::Green).add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(Color::Gray)
                        }),
                    ]),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("←/→", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                        Span::styled(": Switch | ", Style::default().fg(Color::Gray)),
                        Span::styled("Enter", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                        Span::styled(": Confirm | ", Style::default().fg(Color::Gray)),
                        Span::styled("Esc", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                        Span::styled(": Cancel", Style::default().fg(Color::Gray)),
                    ]),
                ];

                let para = Paragraph::new(text)
                    .block(block)
                    .alignment(Alignment::Center);

                f.render_widget(WidgetClear, area);
                f.render_widget(para, area);
            }

            // Help dialog
            if self.show_help {
                let area = centered_rect(90, 30, size);
                let block = Block::default()
                    .borders(Borders::ALL)
                    .border_type(ratatui::widgets::BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Cyan))
                    .title("📚 Help - Keyboard Shortcuts")
                    .title_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));

                let help_text = vec![
                    Line::from(vec![
                        Span::styled("🧭 Navigation", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                    ]),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("  ↑/↓", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                        Span::styled(" - Navigate server list", Style::default().fg(Color::White)),
                    ]),
                    Line::from(vec![
                        Span::styled("  Ctrl+F", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                        Span::styled(" - Toggle Quick Connect focus", Style::default().fg(Color::White)),
                    ]),
                    Line::from(vec![
                        Span::styled("  Enter", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                        Span::styled(" - Connect to selected server or Quick Connect input", Style::default().fg(Color::White)),
                    ]),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("⚡ Actions", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                    ]),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("  e", Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD)),
                        Span::styled(" - Edit selected server", Style::default().fg(Color::White)),
                    ]),
                    Line::from(vec![
                        Span::styled("  n", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                        Span::styled(" - Add new server", Style::default().fg(Color::White)),
                    ]),
                    Line::from(vec![
                        Span::styled("  Del / Ctrl+D", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                        Span::styled(" - Delete selected server", Style::default().fg(Color::White)),
                    ]),
                    Line::from(vec![
                        Span::styled("  h", Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)),
                        Span::styled(" - Show this help dialog", Style::default().fg(Color::White)),
                    ]),
                    Line::from(vec![
                        Span::styled("  q", Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)),
                        Span::styled(" - Quit application", Style::default().fg(Color::White)),
                    ]),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("💡 Tips", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                    ]),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("  • Use Tab to navigate fields in edit/add modes", Style::default().fg(Color::Gray)),
                    ]),
                    Line::from(vec![
                        Span::styled("  • Press Enter to save changes", Style::default().fg(Color::Gray)),
                    ]),
                    Line::from(vec![
                        Span::styled("  • Press Esc to cancel operations", Style::default().fg(Color::Gray)),
                    ]),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("Press any key to close this help", Style::default().fg(Color::Cyan).add_modifier(Modifier::ITALIC)),
                    ]),
                ];

                let para = Paragraph::new(help_text)
                    .block(block)
                    .alignment(Alignment::Left)
                    .wrap(ratatui::widgets::Wrap { trim: false });

                f.render_widget(WidgetClear, area);
                f.render_widget(para, area);
            }
        }
    }

    fn next(&mut self) {
        let i = match self.state.selected() {
            Some(i) => {
                if i >= self.collection.hosts().len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
        self.selected = i;
    }

    fn previous(&mut self) {
        let i = match self.state.selected() {
            Some(i) => {
                if i == 0 {
                    self.collection.hosts().len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
        self.selected = i;
    }

    fn connect(&mut self, terminal: &mut Tui, alias: &String) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(server) = self.collection.get(alias) {
            // Store server details before updating
            let username = server.username.clone();
            let address = server.address.clone();
            let port = server.port;

            // Update last_connect timestamp
            let mut updated_server = Server {
                id: server.id,
                alias: Some(alias.clone()),
                username: username.clone(),
                address: address.clone(),
                port,
                last_connect: server.last_connect.clone(),
            };
            updated_server.set_last_connect_now();

            // Replace the server in collection
            self.collection.insert(alias, updated_server);
            self.collection.save_to_storage(&self.config.server_file_path);

            // Clear alternate screen
            terminal.clear()?;

            // Leave alternate screen
            execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

            // Clear main screen
            execute!(terminal.backend_mut(), Clear(ClearType::All))?;

            // Disable raw mode for SSH
            disable_raw_mode()?;

            // Show cursor for SSH
            execute!(terminal.backend_mut(), Show)?;

            // Run SSH command
            let host = format!("{}@{}", username, address);
            let port_arg = format!("-p{}", port);
            let args = vec![host, port_arg];
            let _ = Command::new(&self.config.ssh_client_app_path)
                .args(args)
                .status();

            // Hide cursor before re-enabling raw mode
            execute!(terminal.backend_mut(), Hide)?;

            // Re-enable raw mode
            enable_raw_mode()?;

            // Re-enter alternate screen
            execute!(terminal.backend_mut(), EnterAlternateScreen)?;

            // Clear alternate screen again
            terminal.clear()?;
        }

        Ok(())
    }
}

pub fn run_app(app: &mut crate::app::App, terminal: &mut Tui) -> Result<(), Box<dyn std::error::Error>> {
    let mut tui_app = TuiApp::new(
        app.get_config().clone(),
        app.get_collection().clone(),
    );

    let result = tui_app.run(terminal);

    // Update the original app with any changes
    *app.get_collection_mut() = tui_app.collection;
    app.save_collection()?;

    result
}
