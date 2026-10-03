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
pub struct StorageFileSummary {
    pub name: String,
    pub kind: String,
    pub size_bytes: u64,
    pub column_family: Option<String>,
    pub level: Option<i32>,
    pub entries: Option<u64>,
    pub deletions: Option<u64>,
    pub state: String,
}

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
    pub query_timeout_ms: u64,
    pub cache_accounted: u64,
    pub memtables_allocated: u64,
    pub directory: String,
    pub files: Vec<StorageFileSummary>,
    pub files_truncated: bool,
}

fn mib(bytes: u64) -> String {
    format!("{:.2} MiB", bytes as f64 / 1_048_576.0)
}

fn storage_details(g: &GraphSummary, view: View, lang: Language) -> String {
    match view {
        View::Memory => format!("{}\n\nmemory.cache: {}\n{}: {}\nmemory.memtables: {}\n{}: {}\nmemory.query: {}\nserver.query_timeout_ms: {} ms\n\n{}",
            lang.choose("PROCESSO OFFLINE • TODOS OS GRAFOS", "OFFLINE PROCESS • ALL GRAPHS"),
            mib(g.cache_budget), lang.choose("Contabilizado", "Accounted"), mib(g.cache_accounted),
            mib(g.memtable_budget), lang.choose("Alocado", "Allocated"), mib(g.memtables_allocated), mib(g.query_budget), g.query_timeout_ms,
            lang.choose("Memtables estão dentro do cache. Orçamento não é RAM reservada. Query: proteção conservadora por RUN e pool compartilhado no servidor. Reservas duram até o fim da transação. Prazo é cooperativo; operações de storage podem excedê-lo. Não é RSS; estes valores são do inspetor offline.\n\nNo servidor: CALL db.memory() YIELD poolLimitBytes, poolReservedBytes RETURN poolLimitBytes, poolReservedBytes",
                "Memtables are inside cache. Budget is not reserved RAM. Query: conservative per-RUN guard plus a shared server pool. Reservations last until transaction end. Deadline is cooperative; storage calls can overrun. Not RSS; these are offline inspector values.\n\nOn the server: CALL db.memory() YIELD poolLimitBytes, poolReservedBytes RETURN poolLimitBytes, poolReservedBytes")),
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
            lang.choose("Índice escalar", "Scalar equality index"),
            if g.index_declarations == 0 { lang.choose("nenhum", "none") } else if g.indexes_ready { lang.choose("pronto", "ready") } else { lang.choose("pendente (scan)", "pending (scan)") },
            g.indexed_fields.join(", "),
            lang.choose("MERGE usa entity_id estável. SET troca campos; null remove.\n\nLEITURAS\nWHERE c.email=$email: índice escalar pronto. AND mantém filtros restantes. OR/NOT/faixa sozinhos: scan.\nCALL db.indexes() YIELD label, property, state RETURN label, property, state\n\nLeituras simples: uma linha por vez; LIMIT para a leitura.\nEscritas, agrupamentos e CALL ainda acumulam linhas.\n\nCSR: graph.project, graph.degree, graph.pagerank, graph.reachable, graph.shortestPath.\nPainel explicativo; não executa consultas.",
                "MERGE uses stable entity_id. SET replaces fields; null removes.\n\nREADS\nWHERE c.email=$email: ready scalar equality index. AND keeps residual filters. Standalone OR/NOT/range: scan.\nCALL db.indexes() YIELD label, property, state RETURN label, property, state\n\nSimple reads process one row at a time; LIMIT stops input.\nWrites, grouped counts and CALL still materialize rows.\n\nCSR: graph.project, graph.degree, graph.pagerank, graph.reachable, graph.shortestPath.\nExplanation panel; no query execution.")),
        View::Graph => unreachable!(),
    }
}

#[derive(Default)]
struct OverviewSelection {
    graph: usize,
    scroll: u16,
    file: usize,
    explain: bool,
}

fn first_data_file(graph: Option<&GraphSummary>) -> usize {
    graph
        .and_then(|g| {
            g.files
                .iter()
                .position(|f| f.kind == "sst" && f.column_family.as_deref() == Some("node"))
                .or_else(|| g.files.iter().position(|f| f.kind == "sst"))
        })
        .unwrap_or(0)
}

fn terminal_text(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            if c.is_control() {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

fn file_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < (1 << 20) {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        mib(bytes)
    }
}

fn file_explanation(file: &StorageFileSummary, lang: Language) -> String {
    let meaning = match file.kind.as_str() {
        "sst" => lang.choose("Dados chave/valor ordenados no disco. Criado por flush ou compactação; preserva registros além da RAM.", "Sorted key/value data on disk. Created by flush or compaction; preserves records beyond RAM."),
        "wal" => lang.choose("Log de escrita antecipada. COMMIT sincroniza o WAL antes de confirmar; reinício recupera dados ainda não enviados a SST.", "Write-ahead log. COMMIT syncs the WAL before acknowledgment; restart recovers data not yet flushed to SST."),
        "manifest" => lang.choose("Histórico da organização dos arquivos SST e níveis do engine. Ajuda a reabrir uma versão consistente.", "Engine history of SST files and levels. Used to reopen a consistent storage version."),
        "pointer" => lang.choose("Aponta para o MANIFEST ativo. Não contém nós ou arestas.", "Points to the active MANIFEST. Contains no nodes or edges."),
        "options" => lang.choose("Configuração persistida do RocksDB. Não é o arquivo brainwire.toml.", "Persisted RocksDB configuration. Separate from brainwire.toml."),
        "lock" => lang.choose("Arquivo de exclusividade. Sua presença sozinha não prova que há um processo ativo.", "Ownership lock file. Its presence alone does not prove an active process."),
        "diagnostic" => lang.choose("Diagnósticos do engine. Diferente dos arquivos WAL numerados.", "Engine diagnostics. Separate from numbered WAL files."),
        "symlink" => lang.choose("Link simbólico: apenas metadados. O destino não foi seguido nem lido.", "Symbolic link: metadata only. Target was neither followed nor read."),
        _ => lang.choose("Arquivo sem classificação conhecida. Conteúdo não foi lido.", "File with no recognized classification. Contents were not read."),
    };
    let family = file.column_family.as_deref().unwrap_or("?");
    let level = file.level.map_or_else(|| "?".into(), |v| format!("L{v}"));
    let counter = |v: Option<u64>| v.map_or_else(|| "?".into(), |n| n.to_string());
    format!("{}\n\n{}: {}\n{}: {}\nCF: {}\n{}: {}\n{}: {}\n{}: {}\n\n{}\n\n{}\n\n{}",
        terminal_text(&file.name), lang.choose("Tipo", "Type"), file.kind,
        lang.choose("Tamanho", "Size"), file_size(file.size_bytes), terminal_text(family),
        lang.choose("Nível LSM", "LSM level"), level,
        lang.choose("Entradas SST", "SST entries"), counter(file.entries),
        lang.choose("Tombstones", "Tombstones"), counter(file.deletions),
        meaning, lang.choose("Entradas SST incluem versões e tombstones; não são contagens de nós vivos.", "SST entries include versions and tombstones; not live node counts."),
        lang.choose("Metadados offline, não atômicos. CF/nível vêm do RocksDB; ? significa indisponível. Não edite ou apague arquivos do engine.", "Offline, non-atomic metadata. CF/level come from RocksDB; ? means unavailable. Do not edit or delete engine files."))
}

fn render_disk(
    frame: &mut Frame,
    graphs: &[GraphSummary],
    state: &mut OverviewSelection,
    area: ratatui::layout::Rect,
    lang: Language,
    no_color: bool,
) {
    let Some(graph) = graphs.get(state.graph) else {
        return;
    };
    state.file = state.file.min(graph.files.len().saturating_sub(1));
    let narrow = area.width < 100;
    let parts = Layout::vertical([
        Constraint::Min(8),
        Constraint::Length(if narrow { 5 } else { 6 }),
    ])
    .split(area);
    let cols = if narrow {
        vec![parts[0], parts[0], parts[0]]
    } else {
        Layout::horizontal([
            Constraint::Length(26),
            Constraint::Percentage(43),
            Constraint::Min(24),
        ])
        .split(parts[0])
        .to_vec()
    };
    if !narrow {
        let visible = cols[0].height.saturating_sub(2) as usize;
        let start = state.graph.saturating_sub(visible.saturating_sub(1));
        let lines = graphs
            .iter()
            .enumerate()
            .skip(start)
            .take(visible)
            .map(|(i, g)| {
                Line::styled(
                    format!(
                        "{} {}",
                        if i == state.graph { ">" } else { " " },
                        terminal_text(&g.name)
                    ),
                    text_style(if i == state.graph { GREEN } else { PALE }, no_color),
                )
            })
            .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(lines).block(panel(
                lang.choose("Grafos • ←→", "Graphs • ←→"),
                GREEN,
                no_color,
            )),
            cols[0],
        );
    }
    if !narrow || !state.explain {
        let visible = cols[1].height.saturating_sub(3) as usize;
        let start = state.file.saturating_sub(visible.saturating_sub(1));
        let mut lines = vec![Line::styled(
            lang.choose(
                " ARQUIVO                   TIPO       TAMANHO",
                " FILE                      TYPE         SIZE",
            ),
            text_style(CYAN, no_color),
        )];
        for (i, file) in graph.files.iter().enumerate().skip(start).take(visible) {
            let name = terminal_text(&file.name)
                .chars()
                .take(24)
                .collect::<String>();
            let size = file_size(file.size_bytes);
            lines.push(Line::styled(
                format!(
                    "{} {name:<24} {:<10} {size:>10}",
                    if i == state.file { ">" } else { " " },
                    file.kind
                ),
                text_style(if i == state.file { GREEN } else { PALE }, no_color),
            ));
        }
        if graph.files.is_empty() {
            lines.push(Line::raw(lang.choose(
                "Nenhum arquivo nesta amostra.",
                "No files in this sample.",
            )));
        }
        let title = format!(
            "{} / {}{}",
            lang.choose("DISCO E ARQUIVOS", "DISK & FILES"),
            terminal_text(&graph.name),
            if graph.files_truncated {
                lang.choose(" • truncado", " • truncated")
            } else {
                ""
            }
        );
        frame.render_widget(
            Paragraph::new(lines).block(panel("", GREEN, no_color).title(title)),
            cols[1],
        );
    }
    if !narrow || state.explain {
        let detail = graph.files.get(state.file).map_or_else(
            || {
                lang.choose("Nenhum arquivo selecionado.", "No file selected.")
                    .to_string()
            },
            |f| file_explanation(f, lang),
        );
        let paragraph = Paragraph::new(detail).wrap(Wrap { trim: false });
        let max = paragraph
            .line_count(cols[2].width.saturating_sub(2))
            .saturating_sub(cols[2].height.saturating_sub(2) as usize)
            .min(u16::MAX as usize) as u16;
        state.scroll = state.scroll.min(max);
        frame.render_widget(
            paragraph.scroll((state.scroll, 0)).block(panel(
                lang.choose("EXPLICAR • F1 • PgUp/PgDn", "EXPLAIN • F1 • PgUp/PgDn"),
                CYAN,
                no_color,
            )),
            cols[2],
        );
    }
    let explanation = format!(
        "{}\n{}\n{}\n{}",
        terminal_text(&graph.directory),
        lang.choose(
            "ESCRITA: WAL + memtables (RAM) -> flush -> SST L0 -> compactação L1+",
            "WRITE: WAL + memtables (RAM) -> flush -> SST L0 -> compaction L1+"
        ),
        lang.choose(
            "LEITURA: cache -> memtables/SST. Índices também são registros persistidos.",
            "READ: cache -> memtables/SST. Indexes are persisted records too."
        ),
        if narrow {
            lang.choose(
                "F1 explica • ←→ grafo • arquivos: metadados offline",
                "F1 explains • ←→ graph • files: offline metadata",
            )
        } else {
            lang.choose("RAM configurada ≠ reservada. Query: proteção por RUN + pool no servidor, não RSS. Valores ? são desconhecidos.", "Configured RAM is not reserved. Query: per-RUN guard + server pool, not RSS. ? values are unknown.")
        }
    );
    frame.render_widget(
        Paragraph::new(explanation)
            .wrap(Wrap { trim: false })
            .block(panel(
                lang.choose("FLUXO DE ARMAZENAMENTO", "STORAGE FLOW"),
                GREEN,
                no_color,
            )),
        parts[1],
    );
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
    state: &mut OverviewSelection,
    view: View,
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

    if view == View::Disk {
        render_disk(frame, graphs, state, sections[1], lang, no_color);
        frame.render_widget(
            Paragraph::new(lang.choose(
                " ←→ grafo • ↑↓ arquivo • F1 explica • Tab painel • q sai",
                " ←→ graph • ↑↓ file • F1 explain • Tab panel • q quit",
            ))
            .block(panel("", GREEN, no_color)),
            sections[2],
        );
        return;
    }
    let selected = state.graph;
    let scroll = &mut state.scroll;
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
    let mut state = OverviewSelection::default();
    let mut view = View::parse(&options.view)?;
    let no_color = std::env::var_os("NO_COLOR").is_some();
    state.graph = match graph {
        Some(name) => graphs
            .iter()
            .position(|g| g.name == name)
            .ok_or_else(|| format!("graph {name:?} does not exist"))?,
        None => 0,
    };
    state.file = first_data_file(graphs.get(state.graph));
    if options.snapshot {
        if !(80..=240).contains(&options.width) || !(24..=80).contains(&options.height) {
            return Err("snapshot size must be 80..240 columns and 24..80 rows".into());
        }
        let backend = TestBackend::new(options.width, options.height);
        let mut terminal = Terminal::new(backend).map_err(|e| e.to_string())?;
        terminal
            .draw(|f| render_overview(f, &graphs, &mut state, view, lang, no_color))
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
            .draw(|f| render_overview(f, &graphs, &mut state, view, lang, no_color))
            .map_err(|e| e.to_string())?;
        if let Event::Key(key) = event::read().map_err(|e| e.to_string())? {
            if key.kind != event::KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Tab => {
                    view = View::from_index(view.index() + 1);
                    state.scroll = 0;
                }
                KeyCode::PageDown => state.scroll = state.scroll.saturating_add(5),
                KeyCode::PageUp => state.scroll = state.scroll.saturating_sub(5),
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::F(1) if view == View::Disk => {
                    state.explain = !state.explain;
                    state.scroll = 0;
                }
                KeyCode::Down if view == View::Disk => {
                    state.file = (state.file + 1).min(
                        graphs
                            .get(state.graph)
                            .map_or(0, |g| g.files.len().saturating_sub(1)),
                    );
                    state.scroll = 0;
                }
                KeyCode::Up if view == View::Disk => {
                    state.file = state.file.saturating_sub(1);
                    state.scroll = 0;
                }
                KeyCode::Left if view == View::Disk => {
                    state.graph = state.graph.saturating_sub(1);
                    state.file = first_data_file(graphs.get(state.graph));
                    state.scroll = 0;
                }
                KeyCode::Right if view == View::Disk => {
                    state.graph = (state.graph + 1).min(graphs.len().saturating_sub(1));
                    state.file = first_data_file(graphs.get(state.graph));
                    state.scroll = 0;
                }
                KeyCode::Down if !graphs.is_empty() => {
                    state.graph = (state.graph + 1).min(graphs.len() - 1);
                    state.file = first_data_file(graphs.get(state.graph));
                    state.scroll = 0;
                }
                KeyCode::Up => {
                    state.graph = state.graph.saturating_sub(1);
                    state.file = first_data_file(graphs.get(state.graph));
                    state.scroll = 0;
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
            let mut state = OverviewSelection {
                scroll: u16::MAX,
                ..OverviewSelection::default()
            };
            terminal
                .draw(|f| render_overview(f, &graphs, &mut state, View::Query, lang, true))
                .unwrap();
            assert!(state.scroll > 0 && state.scroll < u16::MAX);
            let buffer = terminal.backend().buffer();
            let text = (0..24)
                .map(|y| (0..80).map(|x| buffer[(x, y)].symbol()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(text.contains("PgUp/PgDn"));
            assert!(text.contains(lang.choose("consultas.", "execution.")));
            let memory = storage_details(&graphs[0], View::Memory, lang);
            assert!(memory.contains(lang.choose("proteção conservadora", "conservative per-RUN")));
        }
    }

    #[test]
    fn disk_panel_uses_real_file_metadata_and_explains_selected_files() {
        let graphs = vec![GraphSummary {
            name: "storage-test".into(),
            directory: "/tmp/synthetic".into(),
            files_truncated: true,
            files: vec![StorageFileSummary {
                name: "000012.sst".into(),
                kind: "sst".into(),
                size_bytes: 4096,
                column_family: Some("node".into()),
                level: Some(2),
                entries: Some(15),
                deletions: Some(3),
                state: "live_sst_metadata".into(),
            }],
            ..GraphSummary::default()
        }];
        for (width, height) in [(80, 24), (120, 36), (160, 48), (240, 48)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut state = OverviewSelection::default();
            terminal
                .draw(|f| render_overview(f, &graphs, &mut state, View::Disk, Language::En, true))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let text = (0..height)
                .map(|y| {
                    (0..width)
                        .map(|x| buffer[(x, y)].symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join(
                    "
",
                );
            assert!(text.contains("000012.sst"));
            assert!(text.contains("F1"));
            assert!(text.contains("truncated"));
        }
        let detail = file_explanation(&graphs[0].files[0], Language::En);
        assert!(detail.contains("node"));
        assert!(detail.contains("L2"));
        assert!(detail.contains("Sorted key/value"));
        assert!(detail.contains("not live node counts"));
        assert!(!terminal_text("x\u{1b}[31m\n").contains('\u{1b}'));
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
            .draw(|f| {
                render_overview(
                    f,
                    &graphs,
                    &mut OverviewSelection {
                        graph: 1,
                        ..OverviewSelection::default()
                    },
                    View::Graph,
                    Language::En,
                    false,
                )
            })
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
