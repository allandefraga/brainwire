//! Read-only overview of real named local graphs. No synthetic graph data.

use super::*;
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

// The live catalog view belongs to the real overview, not the synthetic demo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OverviewView {
    Graph,
    Query,
    Memory,
    Disk,
    Ontology,
}
impl OverviewView {
    const ALL: [Self; 5] = [
        Self::Graph,
        Self::Query,
        Self::Memory,
        Self::Disk,
        Self::Ontology,
    ];
    fn parse(name: &str) -> Result<Self, String> {
        match name {
            "graph" => Ok(Self::Graph),
            "query" => Ok(Self::Query),
            "memory" => Ok(Self::Memory),
            "disk" => Ok(Self::Disk),
            "ontology" => Ok(Self::Ontology),
            _ => Err("--view must be graph, query, memory, disk or ontology".into()),
        }
    }
    fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|v| *v == self)
            .expect("known view")
    }
    fn from_index(index: usize) -> Self {
        Self::ALL[index % Self::ALL.len()]
    }
    fn display(self, lang: Language) -> &'static str {
        match self {
            Self::Graph => View::Graph.display(lang),
            Self::Query => View::Query.display(lang),
            Self::Memory => View::Memory.display(lang),
            Self::Disk => View::Disk.display(lang),
            Self::Ontology => lang.choose("ONTOLOGIA", "ONTOLOGY"),
        }
    }
}

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
    pub ontology: OntologySummary,
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
    pub commit_sequence: Option<u64>,
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

fn storage_details(g: &GraphSummary, view: OverviewView, lang: Language) -> String {
    match view {
        OverviewView::Memory => format!("{}\n\nmemory.cache: {}\n{}: {}\nmemory.memtables: {}\n{}: {}\nmemory.query: {}\nserver.query_timeout_ms: {} ms\n\n{}",
            lang.choose("PROCESSO OFFLINE • TODOS OS GRAFOS", "OFFLINE PROCESS • ALL GRAPHS"),
            mib(g.cache_budget), lang.choose("Contabilizado", "Accounted"), mib(g.cache_accounted),
            mib(g.memtable_budget), lang.choose("Alocado", "Allocated"), mib(g.memtables_allocated), mib(g.query_budget), g.query_timeout_ms,
            lang.choose("Memtables estão dentro do cache. Orçamento não é RAM reservada. Query: proteção conservadora por RUN e pool compartilhado no servidor. Reservas duram até o fim da transação. Prazo é cooperativo; operações de storage podem excedê-lo. Não é RSS; estes valores são do inspetor offline.\n\nNo servidor: CALL db.memory() YIELD poolLimitBytes, poolReservedBytes RETURN poolLimitBytes, poolReservedBytes",
                "Memtables are inside cache. Budget is not reserved RAM. Query: conservative per-RUN guard plus a shared server pool. Reservations last until transaction end. Deadline is cooperative; storage calls can overrun. Not RSS; these are offline inspector values.\n\nOn the server: CALL db.memory() YIELD poolLimitBytes, poolReservedBytes RETURN poolLimitBytes, poolReservedBytes")),
        OverviewView::Disk => {
            let mut text = lang.choose("ROCKSDB • GRAFO SELECIONADO\nCF       ~keys   mem KiB  L0\n", "ROCKSDB • SELECTED GRAPH\nCF       ~keys   mem KiB  L0\n").to_string();
            for (cf, metrics) in &g.storage {
                let value = |key: &str, divisor: u64| metrics.get(key).copied().flatten().map_or_else(|| "?".into(), |v| (v / divisor).to_string());
                text.push_str(&format!("{cf:<6} {:>7} {:>9} {:>3}\n", value("estimated_keys", 1), value("memtable_bytes", 1024), value("level0_files", 1)));
            }
            text.push_str(lang.choose("\n~keys: estimativa, inclui versões.\nWAL -> memtables -> SST -> compactação", "\n~keys: estimate, includes versions.\nWAL -> memtables -> SST -> compaction"));
            text
        }
        OverviewView::Query => format!("Python -> Bolt -> Cypher\nUNWIND $rows -> MERGE -> SET\n\n{}\n\n{}: {}\n{}\n\n{}: {}\n{}\n\n{}",
            lang.choose("Nó + índices: um WriteBatch.\nCOMMIT sincroniza o WAL.\nFlush cria SST depois.", "Node + indexes: one WriteBatch.\nCOMMIT syncs the WAL.\nFlush creates SST later."),
            lang.choose("Sequência persistida do store", "Persisted store sequence"),
            g.commit_sequence.map_or_else(|| lang.choose("não rastreado", "untracked").to_string(), |sequence| sequence.to_string()),
            lang.choose("Último commit deste store, lido offline. Inclui reservas de IDs e escritas de ontologia; não é contagem de nós nem posição de replicação global.\n\nNo servidor, CALL informa o snapshot da transação:\nCALL db.commitState() YIELD sequence, tracked, scope RETURN sequence, tracked, scope",
                "Latest commit of this store, read offline. Includes ID reservations and ontology writes; not a node count or global replication position.\n\nOn the server, CALL reports the transaction snapshot:\nCALL db.commitState() YIELD sequence, tracked, scope RETURN sequence, tracked, scope"),
            lang.choose("Índice escalar", "Scalar equality index"),
            if g.index_declarations == 0 { lang.choose("nenhum", "none") } else if g.indexes_ready { lang.choose("pronto", "ready") } else { lang.choose("pendente (scan)", "pending (scan)") },
            g.indexed_fields.join(", "),
            lang.choose("MERGE usa entity_id estável. SET troca campos; null remove.\n\nLEITURAS\nWHERE c.email=$email: índice escalar pronto. AND mantém filtros restantes. OR/NOT/faixa sozinhos: scan.\nCALL db.indexes() YIELD label, property, state RETURN label, property, state\n\nLeituras simples: uma linha por vez; LIMIT para a leitura.\nEscritas, agrupamentos e CALL ainda acumulam linhas.\n\nCSR: graph.project, graph.degree, graph.pagerank, graph.reachable, graph.shortestPath.\nPainel explicativo; não executa consultas.",
                "MERGE uses stable entity_id. SET replaces fields; null removes.\n\nREADS\nWHERE c.email=$email: ready scalar equality index. AND keeps residual filters. Standalone OR/NOT/range: scan.\nCALL db.indexes() YIELD label, property, state RETURN label, property, state\n\nSimple reads process one row at a time; LIMIT stops input.\nWrites, grouped counts and CALL still materialize rows.\n\nCSR: graph.project, graph.degree, graph.pagerank, graph.reachable, graph.shortestPath.\nExplanation panel; no query execution.")),
        OverviewView::Graph | OverviewView::Ontology => unreachable!(),
    }
}

#[derive(Default)]
struct OverviewSelection {
    graph: usize,
    node_type: usize,
    scroll: u16,
    file: usize,
    explain: bool,
    all_connections: bool,
    help: bool,
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

pub(super) fn terminal_text(text: &str) -> String {
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

pub(super) fn panel(title: &'static str, color: Color, no_color: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(if no_color {
            Style::default()
        } else {
            Style::default().fg(color).bg(BG)
        })
}

pub(super) fn text_style(color: Color, no_color: bool) -> Style {
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
    view: OverviewView,
    lang: Language,
    no_color: bool,
) {
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .split(frame.area());
    let mut header = vec![
        Span::styled(
            " BRAINWIRE ",
            text_style(GREEN, no_color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("/ {} ", view.display(lang)),
            text_style(CYAN, no_color),
        ),
        Span::raw(if view == OverviewView::Ontology {
            lang.choose(" • esquema offline", " • offline schema")
        } else if graphs.first().is_some_and(|g| g.estimated) {
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
    ];
    if view == OverviewView::Ontology {
        if let Some(graph) = graphs.get(state.graph) {
            let indexes = if graph.index_declarations == 0 {
                lang.choose("sem índices declarados", "no declared indexes")
            } else if graph.indexes_ready {
                lang.choose("índices prontos", "indexes ready")
            } else {
                lang.choose("índices pendentes", "indexes pending")
            };
            header.push(Span::raw(format!(
                " • {} • {indexes}",
                terminal_text(&graph.name)
            )));
        }
    }
    let header = Line::from(header);
    let pages = [
        (OverviewView::Graph, lang.choose("GRAFOS", "GRAPHS")),
        (OverviewView::Query, lang.choose("PIPELINE", "PIPELINE")),
        (OverviewView::Memory, lang.choose("MEMÓRIA", "MEMORY")),
        (OverviewView::Disk, lang.choose("ARQUIVOS", "FILES")),
        (OverviewView::Ontology, lang.choose("ONTOLOGIA", "ONTOLOGY")),
    ];
    let nav = Line::from(
        pages
            .iter()
            .flat_map(|(page, label)| {
                let selected = *page == view;
                [
                    Span::styled(
                        format!(" {} ", label),
                        if selected {
                            text_style(GREEN, no_color)
                                .add_modifier(Modifier::REVERSED | Modifier::BOLD)
                        } else {
                            text_style(PALE, no_color)
                        },
                    ),
                    Span::raw(" "),
                ]
            })
            .collect::<Vec<_>>(),
    );
    frame.render_widget(
        Paragraph::new(vec![header, nav]).block(panel("", GREEN, no_color)),
        sections[0],
    );

    if view == OverviewView::Ontology {
        if let Some(graph) = graphs.get(state.graph) {
            state.node_type = state
                .node_type
                .min(graph.ontology.nodes.len().saturating_sub(1));
            crate::ontology::render(
                frame,
                sections[1],
                &graph.ontology,
                state.node_type,
                &mut state.scroll,
                state.explain,
                state.all_connections,
                lang,
                no_color,
            );
        } else {
            frame.render_widget(
                Paragraph::new(lang.choose("Nenhum grafo encontrado.", "No graphs found."))
                    .block(panel(view.display(lang), GREEN, no_color)),
                sections[1],
            );
        }
        frame.render_widget(
            Paragraph::new(if state.all_connections {
                lang.choose(
                    " ←→ grafo • ↑↓ tipo • a tipos • ? ajuda • PgUp/Dn rola catálogo • Tab páginas • q sai",
                    " ←→ graph • ↑↓ type • a selected type • ? help • PgUp/Dn browse catalog • Tab pages • q quit",
                )
            } else {
                lang.choose(
                    " ←→ grafo • ↑↓ tipo • a catálogo total • F1 detalhes • ? ajuda • PgUp/Dn rola • Tab páginas • q sai",
                    " ←→ graph • ↑↓ type • a full catalog • F1 inspect • ? help • PgUp/Dn scroll • Tab pages • q quit",
                )
            })
            .block(panel("", GREEN, no_color)),
            sections[2],
        );
        if state.help {
            render_page_help(frame, frame.area(), view, lang, no_color);
        }
        return;
    }

    if view == OverviewView::Disk {
        render_disk(frame, graphs, state, sections[1], lang, no_color);
        frame.render_widget(
            Paragraph::new(lang.choose(
                " ←→ grafo • ↑↓ arquivo • F1 explica • ? ajuda • Tab páginas • q sai",
                " ←→ graph • ↑↓ file • F1 explain • ? help • Tab pages • q quit",
            ))
            .block(panel("", GREEN, no_color)),
            sections[2],
        );
        if state.help {
            render_page_help(frame, frame.area(), view, lang, no_color);
        }
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
                    "Contagens estimadas. Tab: pipeline, memória, armazenamento, ontologia. Histórico pendente.",
                    "Estimated counts. Tab: pipeline, memory, storage, ontology. History pending.",
                )
            } else {
                lang.choose(
                    "Contagens exatas. Tab: pipeline, memória, armazenamento, ontologia. Histórico pendente.",
                    "Exact counts. Tab: pipeline, memory, storage, ontology. History pending.",
                )
            }
        )
    } else {
        lang.choose("Nenhum grafo encontrado.", "No graphs found.")
            .to_string()
    };
    if let Some(graph) = graphs.get(selected) {
        if view != OverviewView::Graph {
            details = storage_details(graph, view, lang);
        }
    }
    let title = if view == OverviewView::Graph {
        lang.choose("Grafo selecionado", "Selected graph")
    } else if view == OverviewView::Query {
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
                " ↑↓ grafo • Tab páginas • ? ajuda • q sai • offline",
                " ↑↓ graph • Tab pages • ? help • q quit • offline",
            )
        } else {
            lang.choose(
                " ↑↓ grafo • Tab páginas • ? ajuda • q sai • snapshot: --snapshot  •  offline: pare o servidor primeiro",
                " ↑↓ graph • Tab pages • ? help • q quit • snapshot: --snapshot • offline: stop server first",
            )
        })
        .block(panel("", GREEN, no_color)),
        sections[2],
    );
    if state.help {
        render_page_help(frame, frame.area(), view, lang, no_color);
    }
}

fn render_page_help(
    frame: &mut Frame,
    area: ratatui::layout::Rect,
    view: OverviewView,
    lang: Language,
    no_color: bool,
) {
    use ratatui::widgets::Clear;
    let width = area.width.saturating_sub(4).clamp(1, 88);
    let height = area.height.saturating_sub(2).clamp(1, 24);
    let popup = ratatui::layout::Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    );
    let (title, content) = match view {
        OverviewView::Graph => (
            lang.choose("Grafos: o que estou vendo?", "Graphs: what am I seeing?"),
            lang.choose(
                "Cada linha e um banco nomeado independente. Nos/arestas podem ser exatos ou estimados; confira o indicador no topo.\n\n↑↓ seleciona grafo. Tab muda de pagina. Use a pagina Ontologia para navegar tipos e relacoes permitidas.\n\nEste inspetor e offline e somente leitura. Pare o servidor antes de abrir o armazenamento local.",
                "Each row is an independent named graph. Node/edge counts may be exact or estimated; check the status in the header.\n\n↑↓ selects a graph. Tab changes page. Use Ontology to browse types and allowed relationships.\n\nThis inspector is offline and read-only. Stop the server before opening local storage.",
            ),
        ),
        OverviewView::Query => (
            lang.choose("Pipeline: como a escrita funciona", "Pipeline: how writes work"),
            lang.choose(
                "Este painel explica o caminho Python → Bolt → Cypher e a persistencia. Ele nao executa consultas.\n\nMERGE encontra ou cria pela identidade estavel; SET atualiza propriedades e null remove uma propriedade. O commit sincroniza o WAL; SST e criado depois por flush.\n\nUse o cliente Python ou brainwire cypher para executar. Tab percorre as paginas; ? fecha esta ajuda.",
                "This panel explains Python → Bolt → Cypher and persistence. It does not execute queries.\n\nMERGE finds or creates by stable identity; SET updates properties and null removes one. Commit syncs the WAL; SST files are produced later by flush.\n\nUse the Python client or brainwire cypher to execute. Tab changes pages; ? closes this help.",
            ),
        ),
        OverviewView::Memory => (
            lang.choose("Memoria: entenda os numeros", "Memory: understand the numbers"),
            lang.choose(
                "Configurado e o orcamento; contabilizado/alocado e o contador observado pelo RocksDB. O inspetor offline nao mede RSS nem o uso total do sistema.\n\nmemory.memtables faz parte de memory.cache — nao some os dois. memory.query limita trabalho temporario; o prazo e cooperativo.\n\nTab muda de pagina. O painel explica a configuracao local; nao altera limites.",
                "Configured is a budget; accounted/allocated is what RocksDB reports. The offline inspector does not measure RSS or total system usage.\n\nmemory.memtables is inside memory.cache—do not add them. memory.query limits temporary work; its deadline is cooperative.\n\nTab changes page. This panel explains local settings; it does not change limits.",
            ),
        ),
        OverviewView::Disk => (
            lang.choose("Arquivos: navegar com seguranca", "Files: browse safely"),
            lang.choose(
                "Metadados dos arquivos do engine, nao o conteudo dos registros. SST guarda dados ordenados; WAL permite recuperar commits recentes; MANIFEST acompanha os arquivos SST ativos.\n\n↑↓ seleciona arquivo. ←→ troca o grafo. ? fecha esta ajuda. Nao edite nem apague arquivos do RocksDB.\n\nEsta pagina e offline: pare o servidor antes de inspecionar o diretorio.",
                "Engine file metadata, not record contents. SST stores sorted data; WAL recovers recent commits; MANIFEST tracks active SST files.\n\n↑↓ selects a file. ←→ changes graph. ? closes this help. Do not edit or delete RocksDB files.\n\nThis page is offline: stop the server before inspecting the directory.",
            ),
        ),
        OverviewView::Ontology => (
            lang.choose("Ontologia: tipos e relacoes permitidas", "Ontology: types and allowed relationships"),
            lang.choose(
                "Os tipos e relacoes mostrados sao regras declaradas no schema. Uma relacao permitida nao significa que ja existam arestas gravadas.\n\n↑↓ escolhe tipo; Enter/F1 abre detalhes; a alterna entre relacoes deste tipo e o catalogo completo; PgUp/PgDn rola. ←→ troca o grafo.\n\nCatalogo completo mostra origem, tipo da relacao e destino, preservando a direcao. Tab muda de pagina.",
                "Types and relationships shown are declarations in the schema. An allowed relationship does not mean stored edges already exist.\n\n↑↓ selects a type; Enter/F1 opens details; a toggles between this type's relationships and the complete catalog; PgUp/PgDn scrolls. ←→ changes graph.\n\nThe full catalog shows source, relationship type and target, preserving direction. Tab changes page.",
            ),
        ),
    };
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(format!(
            "{content}\n\n{}",
            lang.choose("? ou Esc fecha a ajuda.", "? or Esc closes help.")
        ))
        .wrap(Wrap { trim: false })
        .block(panel(title, GREEN, no_color)),
        popup,
    );
}

pub fn run_overview(
    options: &Options,
    graphs: Vec<GraphSummary>,
    graph: Option<&str>,
) -> Result<(), String> {
    let lang = Language::parse(&options.lang)?;
    let mut state = OverviewSelection::default();
    let mut view = OverviewView::parse(&options.view)?;
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
            if handle_key(key.code, &mut state, &mut view, &graphs) {
                break;
            }
        }
    }
    Ok(())
}

// Returns true when the user asks to leave the overview.
fn handle_key(
    key: KeyCode,
    state: &mut OverviewSelection,
    view: &mut OverviewView,
    graphs: &[GraphSummary],
) -> bool {
    if state.help && !matches!(key, KeyCode::Char('?') | KeyCode::Esc) {
        return false;
    }
    let mut graph_changed = false;
    match key {
        KeyCode::Char('?') => {
            state.help = !state.help;
            state.scroll = 0;
        }
        KeyCode::Esc if state.help => {
            state.help = false;
            state.scroll = 0;
        }
        KeyCode::Tab => {
            *view = OverviewView::from_index(view.index() + 1);
            state.scroll = 0;
            state.explain = false;
        }
        KeyCode::BackTab => {
            *view = OverviewView::from_index(view.index() + OverviewView::ALL.len() - 1);
            state.scroll = 0;
            state.explain = false;
        }
        KeyCode::PageDown => state.scroll = state.scroll.saturating_add(5),
        KeyCode::PageUp => state.scroll = state.scroll.saturating_sub(5),
        KeyCode::Char('a') if *view == OverviewView::Ontology => {
            state.all_connections = !state.all_connections;
            state.scroll = 0;
        }
        KeyCode::Esc if *view == OverviewView::Ontology && state.explain => {
            state.explain = false;
            state.scroll = 0;
        }
        KeyCode::Char('q') | KeyCode::Esc => return true,
        KeyCode::F(1)
            if matches!(view, OverviewView::Disk | OverviewView::Ontology)
                && !(*view == OverviewView::Ontology && state.all_connections) =>
        {
            state.explain = !state.explain;
            state.scroll = 0;
        }
        KeyCode::Enter if *view == OverviewView::Ontology && !state.all_connections => {
            state.explain = !state.explain;
            state.scroll = 0;
        }
        KeyCode::Down if *view == OverviewView::Ontology => {
            state.node_type = (state.node_type + 1).min(
                graphs
                    .get(state.graph)
                    .map_or(0, |g| g.ontology.nodes.len().saturating_sub(1)),
            );
            state.scroll = 0;
        }
        KeyCode::Up if *view == OverviewView::Ontology => {
            state.node_type = state.node_type.saturating_sub(1);
            state.scroll = 0;
        }
        KeyCode::Down if *view == OverviewView::Disk => {
            state.file = (state.file + 1).min(
                graphs
                    .get(state.graph)
                    .map_or(0, |g| g.files.len().saturating_sub(1)),
            );
            state.scroll = 0;
        }
        KeyCode::Up if *view == OverviewView::Disk => {
            state.file = state.file.saturating_sub(1);
            state.scroll = 0;
        }
        KeyCode::Left if matches!(view, OverviewView::Disk | OverviewView::Ontology) => {
            state.graph = state.graph.saturating_sub(1);
            graph_changed = true;
        }
        KeyCode::Right if matches!(view, OverviewView::Disk | OverviewView::Ontology) => {
            state.graph = (state.graph + 1).min(graphs.len().saturating_sub(1));
            graph_changed = true;
        }
        KeyCode::Down => {
            state.graph = (state.graph + 1).min(graphs.len().saturating_sub(1));
            graph_changed = true;
        }
        KeyCode::Up => {
            state.graph = state.graph.saturating_sub(1);
            graph_changed = true;
        }
        _ => {}
    }
    if graph_changed {
        state.file = first_data_file(graphs.get(state.graph));
        state.node_type = 0;
        state.scroll = 0;
        state.explain = false;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_panel_shows_persisted_store_sequence_and_localized_untracked_state() {
        for lang in [Language::En, Language::PtBr] {
            for sequence in [Some(u64::MAX), None] {
                let graphs = vec![GraphSummary {
                    name: "synthetic-sequence".into(),
                    commit_sequence: sequence,
                    ..GraphSummary::default()
                }];
                let mut terminal = Terminal::new(TestBackend::new(120, 36)).unwrap();
                let mut state = OverviewSelection::default();
                terminal
                    .draw(|f| {
                        render_overview(f, &graphs, &mut state, OverviewView::Query, lang, true)
                    })
                    .unwrap();
                let buffer = terminal.backend().buffer();
                let text = (0..36)
                    .map(|y| {
                        (0..120)
                            .map(|x| buffer[(x, y)].symbol())
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                assert!(
                    text.contains(lang.choose("Sequência persistida", "Persisted store sequence"))
                );
                assert!(text.contains(&sequence.map_or_else(
                    || lang.choose("não rastreado", "untracked").into(),
                    |value| value.to_string(),
                )));
                let details = storage_details(&graphs[0], OverviewView::Query, lang);
                assert!(details.contains("CALL db.commitState() YIELD sequence, tracked, scope RETURN sequence, tracked, scope"));
                assert!(details.contains(lang.choose("reservas de IDs", "ID reservations")));
                assert!(details.contains(lang.choose("replicação global", "global replication")));
                assert!(
                    details.contains(lang.choose("snapshot da transação", "transaction snapshot"))
                );
            }
        }
    }

    #[test]
    fn ontology_navigation_selects_types_resets_on_graph_change_and_closes_inspector() {
        let graphs = vec![
            GraphSummary {
                ontology: OntologySummary {
                    nodes: vec![OntologyNode::default(), OntologyNode::default()],
                    edges: vec![],
                },
                ..GraphSummary::default()
            },
            GraphSummary::default(),
        ];
        let mut state = OverviewSelection::default();
        let mut view = OverviewView::Disk;
        assert!(!handle_key(KeyCode::Tab, &mut state, &mut view, &graphs));
        assert_eq!(view, OverviewView::Ontology);
        for _ in 0..3 {
            handle_key(KeyCode::Down, &mut state, &mut view, &graphs);
        }
        assert_eq!(state.node_type, 1);
        handle_key(KeyCode::Char('a'), &mut state, &mut view, &graphs);
        assert!(state.all_connections);
        handle_key(KeyCode::Char('?'), &mut state, &mut view, &graphs);
        assert!(state.help);
        // Help is reachable even when the catalog has no selected edge inspector.
        handle_key(KeyCode::Down, &mut state, &mut view, &graphs);
        assert_eq!(state.node_type, 1);
        handle_key(KeyCode::Esc, &mut state, &mut view, &graphs);
        assert!(!state.help);
        handle_key(KeyCode::PageDown, &mut state, &mut view, &graphs);
        handle_key(KeyCode::Enter, &mut state, &mut view, &graphs);
        assert!(!state.explain);
        handle_key(KeyCode::Char('a'), &mut state, &mut view, &graphs);
        assert!(!state.all_connections);
        assert_eq!(state.scroll, 0);
        handle_key(KeyCode::Enter, &mut state, &mut view, &graphs);
        assert!(state.explain);
        assert!(!handle_key(KeyCode::Esc, &mut state, &mut view, &graphs));
        assert!(!state.explain);
        handle_key(KeyCode::PageDown, &mut state, &mut view, &graphs);
        handle_key(KeyCode::Right, &mut state, &mut view, &graphs);
        assert_eq!((state.graph, state.node_type, state.scroll), (1, 0, 0));
        handle_key(KeyCode::Down, &mut state, &mut view, &graphs);
        assert_eq!(state.node_type, 0);
        handle_key(KeyCode::Tab, &mut state, &mut view, &graphs);
        assert_eq!(view, OverviewView::Graph);
        handle_key(KeyCode::BackTab, &mut state, &mut view, &graphs);
        assert_eq!(view, OverviewView::Ontology);
        assert!(handle_key(
            KeyCode::Char('q'),
            &mut state,
            &mut view,
            &graphs
        ));
    }

    #[test]
    fn contextual_help_explains_each_offline_page_and_schema_vs_data() {
        let cases = [
            (OverviewView::Graph, "independent", "independente"),
            (OverviewView::Query, "does not", "nao executa"),
            (OverviewView::Memory, "does not measure RSS", "nao mede RSS"),
            (OverviewView::Disk, "Do not edit", "Nao edite"),
            (OverviewView::Ontology, "allowed relationship", "permitida"),
        ];
        for lang in [Language::En, Language::PtBr] {
            for (width, height) in [(80, 24), (160, 48)] {
                for (view, expected_en, expected_pt) in cases {
                    let graphs = vec![GraphSummary::default()];
                    let mut state = OverviewSelection {
                        help: true,
                        all_connections: true,
                        ..OverviewSelection::default()
                    };
                    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                    terminal
                        .draw(|f| render_overview(f, &graphs, &mut state, view, lang, true))
                        .unwrap();
                    let text = terminal
                        .backend()
                        .buffer()
                        .content()
                        .iter()
                        .map(|cell| cell.symbol())
                        .collect::<String>();
                    let expected = if lang == Language::En {
                        expected_en
                    } else {
                        expected_pt
                    };
                    assert!(text.contains(expected), "{view:?}: {text}");
                    assert!(text.contains(if lang == Language::En {
                        "? or Esc closes help"
                    } else {
                        "? ou Esc fecha a ajuda"
                    }));
                }
            }
        }
    }

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
                .draw(|f| render_overview(f, &graphs, &mut state, OverviewView::Query, lang, true))
                .unwrap();
            assert!(state.scroll > 0 && state.scroll < u16::MAX);
            let buffer = terminal.backend().buffer();
            let text = (0..24)
                .map(|y| (0..80).map(|x| buffer[(x, y)].symbol()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(text.contains("PgUp/PgDn"));
            assert!(text.contains(lang.choose("consultas.", "execution.")));
            let memory = storage_details(&graphs[0], OverviewView::Memory, lang);
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
                .draw(|f| {
                    render_overview(
                        f,
                        &graphs,
                        &mut state,
                        OverviewView::Disk,
                        Language::En,
                        true,
                    )
                })
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
                    OverviewView::Graph,
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
