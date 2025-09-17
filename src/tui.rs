use std::io::Stdout;
use std::process::Command;

use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Clear as WidgetClear;
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};

use crate::config::Config;
use crate::server::{Server, ServerCollection};

// 计算居中弹窗矩形区域 — Calculate centered popup rectangle area
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

    pub fn run(&mut self, terminal: &mut Tui) -> anyhow::Result<()> {
        loop {
            terminal.draw(|f| self.ui(f))?;

            if let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                if self.editing.is_some() {
                    // 编辑模式 — Edit mode
                    match key.code {
                        KeyCode::Tab => {
                            self.current_field = (self.current_field + 1) % 4;
                        }
                        KeyCode::Enter => {
                            // 保存 — Save
                            let port: u16 = match self.edit_port.parse() {
                                Ok(p) if p >= 1 => p,
                                _ => {
                                    self.error_message = "⚠️ 端口需在 1 到 65535 之间".to_string();
                                    continue;
                                }
                            };
                            if let Some(idx) = self.editing
                                && let Some(old_alias) = self.collection.hosts().keys().nth(idx)
                            {
                                let old_alias = old_alias.clone();
                                let new_server = Server {
                                    id: None,
                                    alias: Some(self.edit_alias.clone()),
                                    username: self.edit_username.clone(),
                                    address: self.edit_address.clone(),
                                    port,
                                    last_connect: None,
                                };
                                self.collection.remove(old_alias.as_str());
                                self.collection.insert(self.edit_alias.as_str(), new_server);
                                if let Err(e) =
                                    self.collection.save_to_storage(&self.config.server_file_path)
                                {
                                    eprintln!("⚠️ 保存 server 集合失败: {}", e);
                                }
                            }
                            self.editing = None;
                            self.error_message.clear();
                        }
                        KeyCode::Esc => {
                            self.editing = None;
                        }
                        KeyCode::Char(c) => match self.current_field {
                            0 => self.edit_alias.push(c),
                            1 => self.edit_username.push(c),
                            2 => self.edit_address.push(c),
                            3 => {
                                if c.is_ascii_digit() {
                                    self.edit_port.push(c)
                                }
                            }
                            _ => {}
                        },
                        KeyCode::Backspace => match self.current_field {
                            0 => {
                                self.edit_alias.pop();
                            }
                            1 => {
                                self.edit_username.pop();
                            }
                            2 => {
                                self.edit_address.pop();
                            }
                            3 => {
                                self.edit_port.pop();
                            }
                            _ => {}
                        },
                        _ => {}
                    }
                } else if self.deleting.is_some() {
                    // 删除确认 — Delete confirmation
                    match key.code {
                        KeyCode::Left => self.confirm_yes = false,
                        KeyCode::Right => self.confirm_yes = true,
                        KeyCode::Enter => {
                            if self.confirm_yes
                                && let Some(idx) = self.deleting
                            {
                                if let Some(alias) = self.collection.hosts().keys().nth(idx) {
                                    let alias_owned = alias.clone();
                                    self.collection.remove(alias_owned.as_str());
                                }
                                if let Err(e) =
                                    self.collection.save_to_storage(&self.config.server_file_path)
                                {
                                    eprintln!("⚠️ 保存 server 集合失败: {}", e);
                                }
                                // 更新选择项 — Update selection
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
                    // 添加模式 — Add mode
                    if self.add_confirm_stage {
                        // 添加确认 — Add confirmation
                        match key.code {
                            KeyCode::Left => self.add_choice = false,
                            KeyCode::Right => self.add_choice = true,
                            KeyCode::Enter => {
                                if self.add_choice {
                                    let port: u16 = match self.add_port.parse() {
                                        Ok(p) if p >= 1 => p,
                                        _ => {
                                            self.error_message =
                                                "⚠️ 端口需在 1 到 65535 之间".to_string();
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
                                    self.collection.insert(self.add_alias.as_str(), server);
                                    if let Err(e) = self
                                        .collection
                                        .save_to_storage(&self.config.server_file_path)
                                    {
                                        eprintln!("⚠️ 保存 server 集合失败: {}", e);
                                    }
                                    // 更新选择到新服务器 — Update selection to new server
                                    if let Some(pos) = self
                                        .collection
                                        .hosts()
                                        .keys()
                                        .position(|k| k == &self.add_alias)
                                    {
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
                        // 添加输入 — Add input
                        match key.code {
                            KeyCode::Tab => {
                                self.add_current_field = (self.add_current_field + 1) % 4;
                            }
                            KeyCode::Enter => {
                                // 转到确认阶段 — Go to confirmation
                                self.add_confirm_stage = true;
                                self.add_choice = false;
                            }
                            KeyCode::Esc => {
                                self.adding = false;
                            }
                            KeyCode::Char(c) => match self.add_current_field {
                                0 => self.add_alias.push(c),
                                1 => self.add_username.push(c),
                                2 => self.add_address.push(c),
                                3 => {
                                    if c.is_ascii_digit() {
                                        self.add_port.push(c)
                                    }
                                }
                                _ => {}
                            },
                            KeyCode::Backspace => match self.add_current_field {
                                0 => {
                                    self.add_alias.pop();
                                }
                                1 => {
                                    self.add_username.pop();
                                }
                                2 => {
                                    self.add_address.pop();
                                }
                                3 => {
                                    self.add_port.pop();
                                }
                                _ => {}
                            },
                            _ => {}
                        }
                    }
                } else {
                    // 正常模式 — Normal mode
                    if self.show_help {
                        // 帮助对话已打开 —— 仅允许 h 或 esc 关闭它 — Help dialog is open - only allow h or esc to close it
                        match key.code {
                            KeyCode::Char('h')
                            | KeyCode::Char('H')
                            | KeyCode::Char('q')
                            | KeyCode::Char('Q')
                            | KeyCode::Esc => {
                                self.show_help = false;
                            }
                            _ => {
                                // 在帮助打开时忽略其它按键 — Ignore all other keys when help is open
                            }
                        }
                    } else {
                        // 帮助未打开时的正常操作 — Normal operation when help is not open
                        // 处理 Quick Connect 聚焦切换 — Handle Quick Connect focus toggle
                        if key.code == KeyCode::Char('f')
                            && key.modifiers.contains(KeyModifiers::CONTROL)
                        {
                            self.quick_connect_focused = !self.quick_connect_focused;
                            if !self.quick_connect_focused {
                                self.input.clear();
                            }
                            continue;
                        }

                        // 如果 Quick Connect 被聚焦，则只处理与输入相关的按键 — If Quick Connect is focused, only handle input-related keys
                        if self.quick_connect_focused {
                            match key.code {
                                KeyCode::Enter => {
                                    if !self.input.is_empty() {
                                        // 使用输入的别名尝试连接服务器（Quick Connect） — Try to connect to the server with the entered alias (Quick Connect)
                                        if let Some(alias) = self
                                            .collection
                                            .hosts()
                                            .keys()
                                            .find(|k| k == &&self.input)
                                        {
                                            self.connect(terminal, &alias.clone())?;
                                        } else {
                                            self.error_message =
                                                format!("Server '{}' not found", self.input);
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
                            // 当 Quick Connect 未聚焦时的普通键处理 — Normal key handling when Quick Connect is not focused
                            match key.code {
                                KeyCode::Char('q') => return Ok(()),
                                KeyCode::Down => self.next(),
                                KeyCode::Up => self.previous(),
                                KeyCode::Delete => {
                                    self.deleting = Some(self.selected);
                                    self.confirm_yes = false;
                                }
                                KeyCode::Char('d')
                                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                                {
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
                                    if let Some(alias) =
                                        self.collection.hosts().keys().nth(self.selected)
                                    {
                                        let alias_owned = alias.clone();
                                        if let Some(server) =
                                            self.collection.get(alias_owned.as_str())
                                        {
                                            self.editing = Some(self.selected);
                                            self.edit_alias = alias_owned.clone();
                                            self.edit_username = server.username.clone();
                                            self.edit_address = server.address.clone();
                                            self.edit_port = server.port.to_string();
                                            self.current_field = 0;
                                        }
                                    }
                                }
                                KeyCode::Enter => {
                                    if let Some(alias) =
                                        self.collection.hosts().keys().nth(self.selected)
                                    {
                                        // 连接到列表中选定的服务器 — Connect to selected server in list
                                        let alias_owned = alias.clone();
                                        self.connect(terminal, alias_owned.as_str())?;
                                    }
                                }
                                KeyCode::Char('h') | KeyCode::Char('H') => {
                                    self.show_help = true;
                                }
                                // 不要在任意按键时自动聚焦 Quick Connect。 — Do not auto-focus Quick Connect on arbitrary keys.
                                // Quick Connect 仅通过 Ctrl+F 切换；此处忽略其他 Char/Backspace。 — Quick Connect is only toggled via Ctrl+F; ignore stray Char/Backspace here.
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
            // 编辑模式 UI — Edit mode UI
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
            let values =
                [&self.edit_alias, &self.edit_username, &self.edit_address, &self.edit_port];

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

            // 错误信息 — Error message
            if !self.error_message.is_empty() {
                let error = Paragraph::new(self.error_message.as_str())
                    .style(Style::default().fg(Color::Red))
                    .alignment(Alignment::Center)
                    .block(Block::default().borders(Borders::ALL).title("Error"));
                f.render_widget(error, chunks[5]);
            }
        } else if self.adding {
            // 添加模式 UI — Add mode UI
            if self.add_confirm_stage {
                // 添加确认对话框 — Add confirmation dialog
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
                        Span::styled(
                            "No",
                            if !self.add_choice {
                                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                            } else {
                                Style::default()
                            },
                        ),
                        Span::raw("  /  "),
                        Span::styled(
                            "Yes",
                            if self.add_choice {
                                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                            } else {
                                Style::default()
                            },
                        ),
                    ]),
                    Line::from(""),
                    Line::from("←/→: Switch | Enter: Confirm | Esc: Cancel"),
                ];

                // 如有错误则显示添加时的错误信息 — Add error message if present
                if !self.error_message.is_empty() {
                    text.insert(0, Line::from(""));
                    text.insert(
                        0,
                        Line::from(vec![Span::styled(
                            &self.error_message,
                            Style::default().fg(Color::Red),
                        )]),
                    );
                }

                let para = Paragraph::new(text).block(block).alignment(Alignment::Center);

                f.render_widget(WidgetClear, area);
                f.render_widget(para, area);
            } else {
                // 添加输入 UI — Add input UI
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
                let values =
                    [&self.add_alias, &self.add_username, &self.add_address, &self.add_port];

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

                // 错误信息 — Error message
                if !self.error_message.is_empty() {
                    let error = Paragraph::new(self.error_message.as_str())
                        .style(Style::default().fg(Color::Red))
                        .alignment(Alignment::Center)
                        .block(Block::default().borders(Borders::ALL).title("Error"));
                    f.render_widget(error, chunks[5]);
                }
            }
        } else {
            // 正常模式 UI — Normal mode UI
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3), // 标题 — Title
                    Constraint::Length(3), // 输入框 — Input
                    Constraint::Min(5),    // 服务器列表 — Server list
                    Constraint::Length(4), // 状态与帮助 — Status & Help
                ])
                .split(size);

            // 标题 — Title
            let title = Paragraph::new("🚀 HostPilot - SSH Manager")
                .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
                .alignment(Alignment::Center)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_type(ratatui::widgets::BorderType::Rounded)
                        .border_style(Style::default().fg(Color::Blue))
                        .title("Main Menu")
                        .title_style(
                            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
                        ),
                );
            f.render_widget(title, chunks[0]);

            // 带更好样式的输入框 — Input box with better styling
            let input_block = Block::default()
                .borders(Borders::ALL)
                .border_type(ratatui::widgets::BorderType::Rounded)
                .border_style(Style::default().fg(if self.quick_connect_focused {
                    Color::Red
                } else {
                    Color::Yellow
                }))
                .title(if self.quick_connect_focused {
                    "🔍 Quick Connect (FOCUSED)"
                } else {
                    "🔍 Quick Connect"
                })
                .title_style(
                    Style::default()
                        .fg(if self.quick_connect_focused { Color::Red } else { Color::Yellow })
                        .add_modifier(Modifier::BOLD),
                );

            let input = Paragraph::new(self.input.as_str())
                .style(Style::default().fg(if self.quick_connect_focused {
                    Color::Red
                } else {
                    Color::Yellow
                }))
                .block(input_block);
            f.render_widget(input, chunks[1]);

            // 带有增强样式的服务器表格 — Server table with enhanced styling
            let server_count = self.collection.hosts().len();
            let table_block = Block::default()
                .borders(Borders::ALL)
                .border_type(ratatui::widgets::BorderType::Rounded)
                .border_style(Style::default().fg(Color::Green))
                .title(format!("📋 Servers ({})", server_count))
                .title_style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD));

            // 表格表头 — Table headers
            let header_cells = ["#", "Alias", "Username", "Address", "Port", "Last Connect"]
                .iter()
                .map(|h| Cell::from(*h));
            let header = Row::new(header_cells).height(1);

            // 表格行 — Table rows
            let rows: Vec<Row> = self
                .collection
                .hosts()
                .iter()
                .enumerate()
                .map(|(index, (alias, server))| {
                    let is_selected = Some(index) == self.state.selected();
                    let base_style = if is_selected {
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Cyan)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    };

                    let cells = vec![
                        Cell::from(format!("{:2}", index + 1)).style(
                            Style::default().fg(if is_selected {
                                Color::Black
                            } else {
                                Color::Gray
                            }),
                        ),
                        Cell::from(format!("{:<15}", alias.chars().take(15).collect::<String>()))
                            .style(
                                Style::default()
                                    .fg(if is_selected { Color::Black } else { Color::Cyan })
                                    .add_modifier(if is_selected {
                                        Modifier::BOLD
                                    } else {
                                        Modifier::empty()
                                    }),
                            ),
                        Cell::from(format!(
                            "{:<12}",
                            server.username.chars().take(12).collect::<String>()
                        ))
                        .style(Style::default().fg(if is_selected {
                            Color::Black
                        } else {
                            Color::Green
                        })),
                        Cell::from(format!(
                            "{:<20}",
                            server.address.chars().take(20).collect::<String>()
                        ))
                        .style(Style::default().fg(if is_selected {
                            Color::Black
                        } else {
                            Color::Green
                        })),
                        Cell::from(format!("{:>5}", server.port)).style(
                            Style::default().fg(if is_selected {
                                Color::Black
                            } else {
                                Color::Magenta
                            }),
                        ),
                        Cell::from(format!(
                            "{:<19}",
                            server.get_last_connect_display().chars().take(19).collect::<String>()
                        ))
                        .style(Style::default().fg(if is_selected {
                            Color::Black
                        } else {
                            Color::Yellow
                        })),
                    ];

                    Row::new(cells).style(base_style).height(1)
                })
                .collect();

            let table = Table::new(
                rows,
                [
                    Constraint::Length(3),  // #
                    Constraint::Length(16), // Alias
                    Constraint::Length(13), // Username
                    Constraint::Length(21), // Address
                    Constraint::Length(6),  // Port
                    Constraint::Min(20),    // Last Connect
                ],
            )
            .header(header)
            .block(table_block)
            .row_highlight_style(
                Style::default().bg(Color::Cyan).fg(Color::Black).add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▶ ");

            f.render_stateful_widget(table, chunks[2], &mut self.state);

            // 状态与帮助合并显示 — Status and Help combined
            let status_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1), // Status
                    Constraint::Length(2), // Help
                ])
                .split(chunks[3]);

            // 状态栏 — Status bar
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
                .block(
                    Block::default()
                        .borders(Borders::TOP)
                        .border_style(Style::default().fg(Color::Gray)),
                );
            f.render_widget(status, status_chunks[0]);

            // 帮助（更好格式化） — Help with better formatting
            let help_lines = vec![
                Line::from(vec![
                    Span::styled(
                        "Navigation: ",
                        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled("↑/↓", Style::default().fg(Color::Yellow)),
                    Span::styled(" | Connect: ", Style::default().fg(Color::Cyan)),
                    Span::styled("Enter", Style::default().fg(Color::Green)),
                    Span::styled(" | Quick Connect: ", Style::default().fg(Color::Cyan)),
                    Span::styled("Ctrl+F", Style::default().fg(Color::Red)),
                ]),
                Line::from(vec![
                    Span::styled(
                        "Actions: ",
                        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                    ),
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

            let help = Paragraph::new(help_lines).alignment(Alignment::Center).block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(Color::Gray)),
            );
            f.render_widget(help, status_chunks[1]);

            // 带有更好样式的删除确认对话框 — Delete confirmation dialog with better styling
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
                        Span::styled(
                            "❌ No",
                            if !self.confirm_yes {
                                Style::default()
                                    .fg(Color::White)
                                    .bg(Color::Red)
                                    .add_modifier(Modifier::BOLD)
                            } else {
                                Style::default().fg(Color::Gray)
                            },
                        ),
                        Span::raw("     "),
                        Span::styled(
                            "✅ Yes",
                            if self.confirm_yes {
                                Style::default()
                                    .fg(Color::White)
                                    .bg(Color::Green)
                                    .add_modifier(Modifier::BOLD)
                            } else {
                                Style::default().fg(Color::Gray)
                            },
                        ),
                    ]),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled(
                            "←/→",
                            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(": Switch | ", Style::default().fg(Color::Gray)),
                        Span::styled(
                            "Enter",
                            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(": Confirm | ", Style::default().fg(Color::Gray)),
                        Span::styled(
                            "Esc",
                            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(": Cancel", Style::default().fg(Color::Gray)),
                    ]),
                ];

                let para = Paragraph::new(text).block(block).alignment(Alignment::Center);

                f.render_widget(WidgetClear, area);
                f.render_widget(para, area);
            }

            // 帮助对话 — Help dialog
            if self.show_help {
                let area = centered_rect(90, 30, size);
                let block = Block::default()
                    .borders(Borders::ALL)
                    .border_type(ratatui::widgets::BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Cyan))
                    .title("📚 Help - Keyboard Shortcuts")
                    .title_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));

                let help_text = vec![
                    Line::from(vec![Span::styled(
                        "🧭 Navigation",
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    )]),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled(
                            "  ↑/↓",
                            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" - Navigate server list", Style::default().fg(Color::White)),
                    ]),
                    Line::from(vec![
                        Span::styled(
                            "  Ctrl+F",
                            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            " - Toggle Quick Connect focus",
                            Style::default().fg(Color::White),
                        ),
                    ]),
                    Line::from(vec![
                        Span::styled(
                            "  Enter",
                            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            " - Connect to selected server or Quick Connect input",
                            Style::default().fg(Color::White),
                        ),
                    ]),
                    Line::from(""),
                    Line::from(vec![Span::styled(
                        "⚡ Actions",
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    )]),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled(
                            "  e",
                            Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" - Edit selected server", Style::default().fg(Color::White)),
                    ]),
                    Line::from(vec![
                        Span::styled(
                            "  n",
                            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" - Add new server", Style::default().fg(Color::White)),
                    ]),
                    Line::from(vec![
                        Span::styled(
                            "  Del / Ctrl+D",
                            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            " - Delete selected server",
                            Style::default().fg(Color::White),
                        ),
                    ]),
                    Line::from(vec![
                        Span::styled(
                            "  h",
                            Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" - Show this help dialog", Style::default().fg(Color::White)),
                    ]),
                    Line::from(vec![
                        Span::styled(
                            "  q",
                            Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" - Quit application", Style::default().fg(Color::White)),
                    ]),
                    Line::from(""),
                    Line::from(vec![Span::styled(
                        "💡 Tips",
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    )]),
                    Line::from(""),
                    Line::from(vec![Span::styled(
                        "  • Use Tab to navigate fields in edit/add modes",
                        Style::default().fg(Color::Gray),
                    )]),
                    Line::from(vec![Span::styled(
                        "  • Press Enter to save changes",
                        Style::default().fg(Color::Gray),
                    )]),
                    Line::from(vec![Span::styled(
                        "  • Press Esc to cancel operations",
                        Style::default().fg(Color::Gray),
                    )]),
                    Line::from(""),
                    Line::from(vec![Span::styled(
                        "Press any key to close this help",
                        Style::default().fg(Color::Cyan).add_modifier(Modifier::ITALIC),
                    )]),
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

    fn connect(&mut self, terminal: &mut Tui, alias: &str) -> anyhow::Result<()> {
        if let Some(server) = self.collection.get(alias) {
            // 在更新前保存服务器详情 — Store server details before updating
            let username = server.username.clone();
            let address = server.address.clone();
            let port = server.port;

            // 更新 last_connect 时间戳 — Update last_connect timestamp
            let mut updated_server = Server {
                id: server.id,
                alias: Some(alias.to_string()),
                username: username.clone(),
                address: address.clone(),
                port,
                last_connect: server.last_connect.clone(),
            };
            updated_server.set_last_connect_now();

            // 在集合中替换该服务器 — Replace the server in collection
            self.collection.insert(alias, updated_server);
            if let Err(e) = self.collection.save_to_storage(&self.config.server_file_path) {
                eprintln!("⚠️ 保存 server 集合失败: {}", e);
            }

            // 清理备用屏幕 — Clear alternate screen
            terminal.clear()?;

            // 退出备用屏幕 — Leave alternate screen
            execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

            // 清理主屏幕 — Clear main screen
            execute!(terminal.backend_mut(), Clear(ClearType::All))?;

            // 为 SSH 禁用 raw 模式 — Disable raw mode for SSH
            disable_raw_mode()?;

            // 显示 SSH 时的光标 — Show cursor for SSH
            execute!(terminal.backend_mut(), Show)?;

            // 运行 SSH 命令 — Run SSH command
            let host = format!("{}@{}", username, address);
            let port_arg = format!("-p{}", port);
            let args = vec![host, port_arg];
            let _ = Command::new(&self.config.ssh_client_app_path).args(args).status();

            // 在重新启用 raw 模式前隐藏光标 — Hide cursor before re-enabling raw mode
            execute!(terminal.backend_mut(), Hide)?;

            // 重新启用 raw 模式 — Re-enable raw mode
            enable_raw_mode()?;

            // 重新进入备用屏幕 — Re-enter alternate screen
            execute!(terminal.backend_mut(), EnterAlternateScreen)?;

            // 再次清理备用屏幕 — Clear alternate screen again
            terminal.clear()?;
        }

        Ok(())
    }
}

pub fn run_app(app: &mut crate::app::App, terminal: &mut Tui) -> anyhow::Result<()> {
    let mut tui_app = TuiApp::new(app.get_config().clone(), app.get_collection().clone());

    let result = tui_app.run(terminal);

    // 将原始 app 更新为任何修改后的内容 — Update the original app with any changes
    *app.get_collection_mut() = tui_app.collection;
    app.save_collection()?;

    result
}
