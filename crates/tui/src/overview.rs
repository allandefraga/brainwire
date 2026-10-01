//! Read-only overview of real named local graphs. No synthetic graph data.

use super::*;
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
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
    pub indexes_ready: bool,
    pub indexed_fields: Vec<String>,
    pub storage:
        std::collections::BTreeMap<String, std::collections::BTreeMap<String, Option<u64>>>,
    pub cache_budget: u64,
    pub memtable_budget: u64,
    pub query_budget: u64,
    pub cache_accounted: u64,
    pub memtables_allocated: u64,
}

fn mib(bytes: u64) -> String {
    format!("{:.2} MiB", bytes as f64 / 1_048_576.0)
}

fn storage_details(g: &GraphSummary, view: View, lang: Language) -> String {
    match view {
        View::Memory => format!("{}\n\nmemory.cache: {}\n{}: {}\nmemory.memtables: {}\n{}: {}\nmemory.query: {}\n\n{}",
            lang.choose("PROCESSO OFFLINE • TODOS OS GRAFOS", "OFFLINE PROCESS • ALL GRAPHS"),
            mib(g.cache_budget), lang.choose("Contabilizado", "Accounted"), mib(g.cache_accounted),
            mib(g.memtable_budget), lang.choose("Alocado", "Allocated"), mib(g.memtables_allocated), mib(g.query_budget),
            lang.choose("Memtables estão dentro do cache. Orçamento não é RAM reservada. Query: limite ainda não aplicado. Estes valores não são do servidor parado, nem RSS.",
                "Memtables are inside cache. Budget is not reserved RAM. Query limit is not enforced. These are not stopped-server values or RSS.")),
        View::Disk => {
            let mut text = lang.choose("ROCKSDB • GRAFO SELECIONADO\nCF       ~keys   mem KiB  L0\n", "ROCKSDB • SELECTED GRAPH\nCF       ~keys   mem KiB  L0\n").to_string();
            for (cf, metrics) in &g.storage {
                let value = |key: &str, divisor: u64| metrics.get(key).copied().flatten().map_or_else(|| "?".into(), |v| (v / divisor).to_string());
                text.push_str(&format!("{cf:<6} {:>7} {:>9} {:>3}\n", value("estimated_keys", 1), value("memtable_bytes", 1024), value("level0_files", 1)));
            }
            text.push_str(lang.choose("\n~keys: estimativa, inclui versões.\nWAL -> memtables -> SST -> compactação", "\n~keys: estimate, includes versions.\nWAL -> memtables -> SST -> compaction"));
            text
        }
        View::Query => format!("Python -> Bolt -> Cypher\nUNWIND $rows -> MERGE -> SET\n\n{}\n\n{}: {}\n{}\n\n{}",
            lang.choose("Nó + índices: um WriteBatch.\nCOMMIT sincroniza o WAL.\nFlush cria SST depois.", "Node + indexes: one WriteBatch.\nCOMMIT syncs the WAL.\nFlush creates SST later."),
            lang.choose("Índice texto/bool", "String/bool index"),
            if g.index_declarations == 0 { lang.choose("nenhum", "none") } else if g.indexes_ready { lang.choose("pronto", "ready") } else { lang.choose("pendente (scan)", "pending (scan)") },
            g.indexed_fields.join(", "),
            lang.choose("MERGE usa entity_id estável. SET troca campos; null remove. Padrão {campo:$v} usa índice; WHERE ainda faz scan. Painel explicativo; não executa consultas.",
                "MERGE uses stable entity_id. SET replaces fields; null removes. Pattern {field:$v} uses index; WHERE still scans. Explanation panel; no query execution.")),
        View::Graph => unreachable!(),
    }
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
    view: View,
    lang: Language,
    no_color: bool,
    scroll: &mut u16,
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
    let compact = columns[0].width < 48;
    let heading = if compact {
        lang.choose(
            " GRAFO               NÓS ARESTAS",
            " GRAPH              NODES  EDGES",
        )
    } else {
        lang.choose(
            " GRAFO                     NÓS      ARESTAS",
            " GRAPH                     NODES    EDGES",
        )
    };
    let mut lines = vec![Line::styled(heading, text_style(CYAN, no_color))];
    for (index, graph) in graphs.iter().enumerate().skip(start).take(visible) {
        let marker = if index == selected { "▶" } else { " " };
        let line = if compact {
            format!(
                "{marker} {:<16} {:>7} {:>8}",
                graph.name.chars().take(16).collect::<String>(),
                graph.nodes,
                graph.edges
            )
        } else {
            format!(
                "{marker} {:<24} {:>8} {:>10}",
                graph.name, graph.nodes, graph.edges
            )
        };
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

    let mut details = if let Some(graph) = graphs.get(selected) {
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
                    "Contagens estimadas. Tab: pipeline, memória, armazenamento. Histórico pendente.",
                    "Estimated counts. Tab: pipeline, memory, storage. History pending.",
                )
            } else {
                lang.choose(
                    "Contagens exatas. Tab: pipeline, memória, armazenamento. Histórico pendente.",
                    "Exact counts. Tab: pipeline, memory, storage. History pending.",
                )
            }
        )
    } else {
        lang.choose("Nenhum grafo encontrado.", "No graphs found.")
            .to_string()
    };
    if let Some(graph) = graphs.get(selected) {
        if view != View::Graph {
            details = storage_details(graph, view, lang);
        }
    }
    let title = if view == View::Graph {
        lang.choose("Grafo selecionado", "Selected graph")
    } else if view == View::Query {
        lang.choose("PIPELINE DE ESCRITA", "WRITE PIPELINE")
    } else {
        view.display(lang)
    };
    let paragraph = Paragraph::new(details).wrap(Wrap { trim: false });
    let height = columns[1].height.saturating_sub(2) as usize;
    let max_scroll = paragraph
        .line_count(columns[1].width.saturating_sub(2))
        .saturating_sub(height)
        .min(u16::MAX as usize) as u16;
    *scroll = (*scroll).min(max_scroll);
    let title = if max_scroll > 0 {
        format!("{title} • PgUp/PgDn")
    } else {
        title.to_string()
    };
    frame.render_widget(
        paragraph
            .scroll((*scroll, 0))
            .block(panel("", CYAN, no_color).title(title)),
        columns[1],
    );
    frame.render_widget(
        Paragraph::new(if frame.area().width < 100 {
            lang.choose(
                " ↑↓ grafo • Tab painel • q sai • offline",
                " ↑↓ graph • Tab panel • q quit • offline",
            )
        } else {
            lang.choose(
                " ↑↓ grafo • Tab painel • q sai • snapshot: --snapshot  •  offline: pare o servidor primeiro",
                " ↑↓ graph • Tab panel • q quit • snapshot: --snapshot  •  offline: stop the server first",
            )
        })
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
    let mut scroll = 0;
    let mut view = View::parse(&options.view)?;
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
            .draw(|f| render_overview(f, &graphs, selected, view, lang, no_color, &mut scroll))
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
            .draw(|f| render_overview(f, &graphs, selected, view, lang, no_color, &mut scroll))
            .map_err(|e| e.to_string())?;
        if let Event::Key(key) = event::read().map_err(|e| e.to_string())? {
            if key.kind != event::KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Tab => {
                    view = View::from_index(view.index() + 1);
                    scroll = 0;
                }
                KeyCode::PageDown => scroll = scroll.saturating_add(5),
                KeyCode::PageUp => scroll = scroll.saturating_sub(5),
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Down if !graphs.is_empty() => {
                    selected = (selected + 1).min(graphs.len() - 1);
                    scroll = 0;
                }
                KeyCode::Up => {
                    selected = selected.saturating_sub(1);
                    scroll = 0;
                }
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
    fn pipeline_overflow_scrolls_to_explanation_and_memory_never_claims_live_usage() {
        let graphs = vec![GraphSummary {
            name: "ingestion".into(),
            index_declarations: 30,
            indexes_ready: true,
            indexed_fields: (0..30).map(|i| format!("Customer.field_{i}")).collect(),
            ..GraphSummary::default()
        }];
        for lang in [Language::En, Language::PtBr] {
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
            let mut scroll = u16::MAX;
            terminal
                .draw(|f| render_overview(f, &graphs, 0, View::Query, lang, true, &mut scroll))
                .unwrap();
            assert!(scroll > 0 && scroll < u16::MAX);
            let buffer = terminal.backend().buffer();
            let text = (0..24)
                .map(|y| (0..80).map(|x| buffer[(x, y)].symbol()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(text.contains("PgUp/PgDn"));
            assert!(text.contains(lang.choose("consultas.", "execution.")));
            let memory = storage_details(&graphs[0], View::Memory, lang);
            assert!(
                memory.contains(lang.choose("limite ainda não aplicado", "limit is not enforced"))
            );
        }
    }

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
                ..GraphSummary::default()
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
                ..GraphSummary::default()
            },
        ];
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal
            .draw(|f| render_overview(f, &graphs, 1, View::Graph, Language::En, false, &mut 0))
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
