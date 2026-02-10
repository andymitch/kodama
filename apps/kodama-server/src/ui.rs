use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Row, Table},
    Frame,
};

use kodama::server::PeerRole;

use crate::app::App;

pub fn draw(f: &mut Frame, app: &App) {
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Min(5),
        Constraint::Length(1),
    ])
    .split(f.area());

    draw_header(f, app, chunks[0]);
    draw_stats(f, app, chunks[1]);
    draw_main(f, app, chunks[2]);
    draw_footer(f, chunks[3]);
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let key_display = if app.server_public_key.len() > 20 {
        format!("{}...", &app.server_public_key[..20])
    } else {
        app.server_public_key.clone()
    };

    let header = Paragraph::new(Line::from(vec![
        Span::styled(" Key: ", Style::default().fg(Color::DarkGray)),
        Span::styled(key_display, Style::default().fg(Color::Cyan)),
        Span::raw("  "),
        Span::styled("Uptime: ", Style::default().fg(Color::DarkGray)),
        Span::styled(app.uptime_str(), Style::default().fg(Color::Green)),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Kodama Server ")
            .title_style(Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
    );
    f.render_widget(header, area);
}

fn draw_stats(f: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![
        Span::styled(" Cameras: ", Style::default().fg(Color::DarkGray)),
        Span::styled(app.stats.cameras_connected.to_string(), Style::default().fg(Color::Yellow)),
        Span::raw("  "),
        Span::styled("Clients: ", Style::default().fg(Color::DarkGray)),
        Span::styled(app.stats.clients_connected.to_string(), Style::default().fg(Color::Yellow)),
        Span::raw("  "),
        Span::styled("Rx: ", Style::default().fg(Color::DarkGray)),
        Span::styled(app.stats.frames_received.to_string(), Style::default().fg(Color::White)),
        Span::raw("  "),
        Span::styled("Tx: ", Style::default().fg(Color::DarkGray)),
        Span::styled(app.stats.frames_broadcast.to_string(), Style::default().fg(Color::White)),
    ];

    if let Some(ref ss) = app.storage_stats {
        spans.push(Span::raw("  "));
        spans.push(Span::styled("Stored: ", Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(
            format!("{} MB", ss.bytes_used / (1024 * 1024)),
            Style::default().fg(Color::White),
        ));
    }

    let stats = Paragraph::new(Line::from(spans))
        .block(Block::default().borders(Borders::ALL).title(" Stats "));
    f.render_widget(stats, area);
}

fn draw_main(f: &mut Frame, app: &App, area: Rect) {
    let peer_height = (app.peers.len() as u16 + 2).max(3).min(10);
    let chunks = Layout::vertical([
        Constraint::Length(peer_height),
        Constraint::Min(3),
    ])
    .split(area);

    draw_peers(f, app, chunks[0]);
    draw_log(f, app, chunks[1]);
}

fn draw_peers(f: &mut Frame, app: &App, area: Rect) {
    if app.peers.is_empty() {
        let p = Paragraph::new(Line::from(vec![Span::styled(
            " No peers connected",
            Style::default().fg(Color::DarkGray),
        )]))
        .block(Block::default().borders(Borders::ALL).title(" Peers "));
        f.render_widget(p, area);
        return;
    }

    let rows: Vec<Row> = app
        .peers
        .iter()
        .map(|(key, role)| {
            let (role_str, color) = match role {
                PeerRole::Camera => ("Camera", Color::Yellow),
                PeerRole::Client => ("Client", Color::Cyan),
            };
            let key_str = format!("{}", key);
            let key_short = if key_str.len() > 30 {
                format!("{}...", &key_str[..30])
            } else {
                key_str
            };
            Row::new(vec![format!(" [{}]", role_str), key_short])
                .style(Style::default().fg(color))
        })
        .collect();

    let table = Table::new(rows, [Constraint::Length(10), Constraint::Min(20)])
        .block(Block::default().borders(Borders::ALL).title(" Peers "));
    f.render_widget(table, area);
}

fn draw_log(f: &mut Frame, app: &App, area: Rect) {
    let inner_height = area.height.saturating_sub(2) as usize;
    let skip = app.log_messages.len().saturating_sub(inner_height);
    let lines: Vec<Line> = app
        .log_messages
        .iter()
        .skip(skip)
        .map(|msg| Line::from(Span::raw(format!(" {}", msg))))
        .collect();

    let log = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(" Log "));
    f.render_widget(log, area);
}

fn draw_footer(f: &mut Frame, area: Rect) {
    let footer = Paragraph::new(Line::from(vec![
        Span::styled(" [q]", Style::default().fg(Color::Yellow)),
        Span::styled(" Quit", Style::default().fg(Color::DarkGray)),
    ]));
    f.render_widget(footer, area);
}
