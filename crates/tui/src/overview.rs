//! Read-only overview of real named local graphs. No synthetic graph data.

use super::*;
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphSummary {
    pub name: String,
    pub nodes: u64,
    pub edges: u64,
    pub estimated: bool,
    pub ontology_version: Option<u32>,
    pub node_types: usize,
    pub edge_types: usize,
    pub node_labels: Vec<String>,
    pub edge_labels: Vec<String>,
    pub index_declarations: usize,
    pub timeline_streams: usize,
}

fn panel(title: &'static str, color: Color, no_color: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(if no_color {
            Style::default()
        } else {
            Style::default().fg(color).bg(BG)
        })
}

fn text_style(color: Color, no_color: bool) -> Style {
    if no_color {
        Style::default()
    } else {
        Style::default().fg(color).bg(BG)
    }
}

fn render_overview(
    frame: &mut Frame,
    graphs: &[GraphSummary],
    selected: usize,
    lang: Language,
    no_color: bool,
) {
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .split(frame.area());
    let header = Line::from(vec![
        Span::styled(
            " BRAINWIRE ",
            text_style(GREEN, no_color).add_modifier(Modifier::BOLD),
        ),
        Span::raw(if graphs.first().is_some_and(|g| g.estimated) {
            lang.choose(
                "/ grafos locais • estimativas RocksDB • somente leitura",
                "/ local graphs • RocksDB estimates • read only",
            )
        } else {
            lang.choose(
                "/ grafos locais • dados reais • somente leitura",
                "/ local graphs • real data • read only",
            )
        }),
    ]);
    frame.render_widget(
        Paragraph::new(header).block(panel("", GREEN, no_color)),
        sections[0],
    );

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(48), Constraint::Percentage(52)])
        .split(sections[1]);
    let visible = columns[0].height.saturating_sub(3) as usize;
    let start = selected.saturating_sub(visible.saturating_sub(1));
    let mut lines = vec![Line::styled(
        lang.choose(
            " GRAFO                     NÓS      ARESTAS",
            " GRAPH                     NODES    EDGES",
        ),
        text_style(CYAN, no_color),
    )];
    for (index, graph) in graphs.iter().enumerate().skip(start).take(visible) {
        let marker = if index == selected { "▶" } else { " " };
        let line = format!(
            "{marker} {:<24} {:>8} {:>10}",
            graph.name, graph.nodes, graph.edges
        );
        lines.push(Line::styled(
            line,
            if index == selected {
                text_style(GREEN, no_color).add_modifier(Modifier::BOLD)
            } else {
                text_style(PALE, no_color)
            },
        ));
    }
    frame.render_widget(
        Paragraph::new(lines).block(panel(
            lang.choose("Grafos nomeados", "Named graphs"),
            GREEN,
            no_color,
        )),
        columns[0],
    );

    let details = if let Some(graph) = graphs.get(selected) {
        let ontology = graph.ontology_version.map_or_else(
            || lang.choose("nenhuma", "none").to_string(),
            |version| format!("v{version}"),
        );
        format!(
            "{}\n\n{}: {}\n{}: {}\n{}: {}\n{}: {}\n{}: {}\n\n{}: {}\n{}: {}\n{}: {}\n{}: {}\n\n{}",
            graph.name,
            lang.choose("Nós", "Nodes"),
            graph.nodes,
            lang.choose("Arestas", "Edges"),
            graph.edges,
            lang.choose("Ontologia", "Ontology"),
            ontology,
            lang.choose("Tipos de nó", "Node types"),
            graph.node_types,
            lang.choose("Tipos de aresta", "Edge types"),
            graph.edge_types,
            lang.choose("Rótulos", "Labels"),
            if graph.node_labels.is_empty() {
                "—".to_string()
            } else {
                graph.node_labels.join(", ")
            },
            lang.choose("Relações", "Relationships"),
            if graph.edge_labels.is_empty() {
                "—".to_string()
            } else {
                graph.edge_labels.join(", ")
            },
            lang.choose("Índices declarados", "Declared indexes"),
            graph.index_declarations,
            lang.choose("Fluxos de timeline", "Timeline streams"),
            graph.timeline_streams,
            if graph.estimated {
                lang.choose(
                    "Contagens estimadas pelo RocksDB. Índices e histórico ainda não estão implementados.",
                    "RocksDB estimated counts. Indexes and history are not implemented yet."
                )
            } else {
                lang.choose(
                    "Contagens exatas. Índices secundários e histórico temporal ainda não estão implementados.",
                    "Exact counts. Secondary indexes and temporal history are not implemented yet."
                )
            }
        )
    } else {
        lang.choose("Nenhum grafo encontrado.", "No graphs found.")
            .to_string()
    };
    frame.render_widget(
        Paragraph::new(details)
            .wrap(Wrap { trim: false })
            .block(panel(
                lang.choose("Grafo selecionado", "Selected graph"),
                CYAN,
                no_color,
            )),
        columns[1],
    );
    frame.render_widget(
        Paragraph::new(lang.choose(
            " ↑↓ seleciona  •  q/Esc sai  •  snapshot: --snapshot  •  offline: pare o servidor primeiro",
            " ↑↓ select  •  q/Esc quit  •  snapshot: --snapshot  •  offline: stop the server first",
        ))
        .block(panel("", GREEN, no_color)),
        sections[2],
    );
}

pub fn run_overview(
    options: &Options,
    graphs: Vec<GraphSummary>,
    graph: Option<&str>,
) -> Result<(), String> {
    let lang = Language::parse(&options.lang)?;
    let no_color = std::env::var_os("NO_COLOR").is_some();
    let mut selected = match graph {
        Some(name) => graphs
            .iter()
            .position(|g| g.name == name)
            .ok_or_else(|| format!("graph {name:?} does not exist"))?,
        None => 0,
    };
    if options.snapshot {
        if !(80..=240).contains(&options.width) || !(24..=80).contains(&options.height) {
            return Err("snapshot size must be 80..240 columns and 24..80 rows".into());
        }
        let backend = TestBackend::new(options.width, options.height);
        let mut terminal = Terminal::new(backend).map_err(|e| e.to_string())?;
        terminal
            .draw(|f| render_overview(f, &graphs, selected, lang, no_color))
            .map_err(|e| e.to_string())?;
        let buffer = terminal.backend().buffer();
        for y in 0..options.height {
            let mut line = String::new();
            for x in 0..options.width {
                line.push_str(buffer[(x, y)].symbol());
            }
            println!("{}", line.trim_end());
        }
        return Ok(());
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("interactive shell requires a terminal; use --snapshot for plain text".into());
    }
    let _guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal: Terminal<CrosstermBackend<Stdout>> =
        Terminal::new(backend).map_err(|e| e.to_string())?;
    loop {
        terminal
            .draw(|f| render_overview(f, &graphs, selected, lang, no_color))
            .map_err(|e| e.to_string())?;
        if let Event::Key(key) = event::read().map_err(|e| e.to_string())? {
            if key.kind != event::KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Down if !graphs.is_empty() => {
                    selected = (selected + 1).min(graphs.len() - 1)
                }
                KeyCode::Up => selected = selected.saturating_sub(1),
                _ => {}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_overview_renders_multiple_graphs_and_selected_details() {
        let graphs = vec![
            GraphSummary {
                name: "knowledge".into(),
                nodes: 0,
                edges: 0,
                estimated: false,
                ontology_version: None,
                node_types: 0,
                edge_types: 0,
                node_labels: vec![],
                edge_labels: vec![],
                index_declarations: 0,
                timeline_streams: 0,
            },
            GraphSummary {
                name: "tickets".into(),
                nodes: 42,
                edges: 17,
                estimated: false,
                ontology_version: Some(3),
                node_types: 2,
                edge_types: 1,
                node_labels: vec!["Ticket".into(), "User".into()],
                edge_labels: vec!["ASSIGNED_TO".into()],
                index_declarations: 1,
                timeline_streams: 2,
            },
        ];
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal
            .draw(|f| render_overview(f, &graphs, 1, Language::En, false))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let text = (0..24)
            .map(|y| {
                (0..100)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("knowledge"));
        assert!(text.contains("tickets"));
        assert!(text.contains("v3"));
        assert!(text.contains("Ticket"));
        assert!(text.contains("Declared indexes"));
        assert!(text.contains("42"));
        assert!(text.contains("real data"));
    }
}
