//! Shared read-only workspace; each pane keeps its own sample and selection.
use crate::overview::{panel, terminal_text};
use crate::*;
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph, Wrap},
    Frame,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectedView {
    Ontology,
    Memory,
    Indexes,
    Graph,
}
impl ConnectedView {
    fn index(self) -> usize {
        match self {
            Self::Ontology => 0,
            Self::Memory => 1,
            Self::Indexes => 2,
            Self::Graph => 3,
        }
    }
    fn from_index(index: usize) -> Self {
        [Self::Ontology, Self::Memory, Self::Indexes, Self::Graph][index % 4]
    }
    fn name(self, lang: Language) -> &'static str {
        match self {
            Self::Ontology => lang.choose("Ontologia", "Ontology"),
            Self::Memory => lang.choose("Memória", "Memory"),
            Self::Indexes => lang.choose("Índices", "Indexes"),
            Self::Graph => lang.choose("Grafo", "Graph"),
        }
    }
}
#[derive(Clone, Debug)]
pub enum ConnectedSnapshot {
    Ontology(OntologySnapshot),
    Memory(MemorySnapshot),
    Indexes(IndexesSnapshot),
    Graph(GraphSnapshot),
}

struct Workspace {
    active: ConnectedView,
    ontology: Option<connected::State>,
    memory: Option<connected_memory::State>,
    indexes: Option<connected_indexes::State>,
    graph: Option<connected_graph::State>,
    attempted: [bool; 4],
    initial_errors: [Option<String>; 4],
    help: bool,
}
impl Workspace {
    fn new() -> Self {
        Self {
            active: ConnectedView::Ontology,
            ontology: None,
            memory: None,
            indexes: None,
            graph: None,
            attempted: [false; 4],
            initial_errors: std::array::from_fn(|_| None),
            help: false,
        }
    }
    fn refresh(
        &mut self,
        fetch: &mut impl FnMut(ConnectedView) -> Result<ConnectedSnapshot, String>,
    ) {
        let view = self.active;
        self.attempted[view.index()] = true;
        let result = fetch(view).and_then(|snapshot| match (view, &snapshot) {
            (ConnectedView::Ontology, ConnectedSnapshot::Ontology(_))
            | (ConnectedView::Memory, ConnectedSnapshot::Memory(_))
            | (ConnectedView::Indexes, ConnectedSnapshot::Indexes(_))
            | (ConnectedView::Graph, ConnectedSnapshot::Graph(_)) => Ok(snapshot),
            _ => Err("server returned a snapshot for a different pane".into()),
        });
        match result {
            Ok(ConnectedSnapshot::Ontology(snapshot)) => match &mut self.ontology {
                Some(state) => state.refresh(Ok(snapshot)),
                None => self.ontology = Some(connected::State::new(snapshot)),
            },
            Ok(ConnectedSnapshot::Memory(snapshot)) => match &mut self.memory {
                Some(state) => state.refresh(Ok(snapshot)),
                None => self.memory = Some(connected_memory::State::new(snapshot)),
            },
            Ok(ConnectedSnapshot::Indexes(snapshot)) => match &mut self.indexes {
                Some(state) => state.refresh(Ok(snapshot)),
                None => self.indexes = Some(connected_indexes::State::new(snapshot)),
            },
            Ok(ConnectedSnapshot::Graph(snapshot)) => match &mut self.graph {
                Some(state) => state.refresh(Ok(snapshot)),
                None => self.graph = Some(connected_graph::State::new(snapshot)),
            },
            Err(error) => {
                match view {
                    ConnectedView::Ontology => {
                        if let Some(state) = &mut self.ontology {
                            state.refresh(Err(error.clone()));
                        }
                    }
                    ConnectedView::Graph => {
                        if let Some(state) = &mut self.graph {
                            state.refresh(Err(error.clone()));
                        }
                    }
                    ConnectedView::Memory => {
                        if let Some(state) = &mut self.memory {
                            state.refresh(Err(error.clone()));
                        }
                    }
                    ConnectedView::Indexes => {
                        if let Some(state) = &mut self.indexes {
                            state.refresh(Err(error.clone()));
                        }
                    }
                }
                self.initial_errors[view.index()] = Some(error);
                return;
            }
        }
        self.initial_errors[view.index()] = None;
    }
    fn select(
        &mut self,
        view: ConnectedView,
        fetch: &mut impl FnMut(ConnectedView) -> Result<ConnectedSnapshot, String>,
    ) {
        self.active = view;
        if !self.attempted[view.index()] {
            self.refresh(fetch);
        }
    }
    /// Returns true only when the user exits the workspace.
    fn handle(
        &mut self,
        key: KeyEvent,
        fetch: &mut impl FnMut(ConnectedView) -> Result<ConnectedSnapshot, String>,
    ) -> bool {
        if key.kind != event::KeyEventKind::Press {
            return false;
        }
        if self.help {
            if matches!(key.code, KeyCode::Char('?') | KeyCode::Esc) {
                self.help = false;
            }
            return false;
        }
        match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('?') => self.help = true,
            KeyCode::Tab => self.select(
                ConnectedView::from_index(
                    self.active.index()
                        + if key.modifiers.contains(KeyModifiers::SHIFT) {
                            3
                        } else {
                            1
                        },
                ),
                fetch,
            ),
            KeyCode::BackTab => {
                self.select(ConnectedView::from_index(self.active.index() + 3), fetch)
            }
            KeyCode::Char('1'..='4') => {
                if let KeyCode::Char(c) = key.code {
                    self.select(ConnectedView::from_index((c as u8 - b'1') as usize), fetch);
                }
            }
            KeyCode::Char('r') => self.refresh(fetch),
            _ => match self.active {
                ConnectedView::Ontology => {
                    if let Some(state) = &mut self.ontology {
                        match key.code {
                            KeyCode::Esc if state.inspect => {
                                state.inspect = false;
                                state.scroll = 0;
                            }
                            KeyCode::Esc => return true,
                            KeyCode::Enter | KeyCode::F(1) => {
                                state.inspect = !state.inspect;
                                state.scroll = 0;
                            }
                            KeyCode::Up => {
                                state.selected = state.selected.saturating_sub(1);
                                state.scroll = 0;
                            }
                            KeyCode::Down => {
                                state.selected = (state.selected + 1)
                                    .min(state.snapshot.ontology.nodes.len().saturating_sub(1));
                                state.scroll = 0;
                            }
                            KeyCode::PageUp => state.scroll = state.scroll.saturating_sub(5),
                            KeyCode::PageDown => state.scroll = state.scroll.saturating_add(5),
                            _ => {}
                        }
                    } else if key.code == KeyCode::Esc {
                        return true;
                    }
                }
                ConnectedView::Indexes => {
                    if let Some(state) = &mut self.indexes {
                        match key.code {
                            KeyCode::Esc => return true,
                            KeyCode::Up => state.selected = state.selected.saturating_sub(1),
                            KeyCode::Down => {
                                state.selected = (state.selected + 1)
                                    .min(state.snapshot.indexes.len().saturating_sub(1))
                            }
                            KeyCode::PageUp => state.selected = state.selected.saturating_sub(5),
                            KeyCode::PageDown => {
                                state.selected = (state.selected + 5)
                                    .min(state.snapshot.indexes.len().saturating_sub(1))
                            }
                            _ => {}
                        }
                    } else if key.code == KeyCode::Esc {
                        return true;
                    }
                }
                ConnectedView::Graph => {
                    if key.code == KeyCode::Esc {
                        return true;
                    }
                    if let Some(state) = &mut self.graph {
                        state.handle(key.code);
                    }
                }
                ConnectedView::Memory => {
                    if key.code == KeyCode::Esc {
                        return true;
                    }
                }
            },
        }
        false
    }
}
fn render(
    frame: &mut Frame,
    workspace: &mut Workspace,
    address: &str,
    graph: &str,
    lang: Language,
    no_color: bool,
) {
    let area = frame.area();
    let content = if area.width >= 120 {
        let columns = Layout::horizontal([Constraint::Length(20), Constraint::Min(0)]).split(area);
        let mut lines = vec![
            Line::raw(lang.choose(" SOMENTE LEITURA", " READ ONLY")),
            Line::raw(""),
        ];
        for view in [
            ConnectedView::Ontology,
            ConnectedView::Memory,
            ConnectedView::Indexes,
            ConnectedView::Graph,
        ] {
            let selected = workspace.active == view;
            lines.push(Line::from(Span::styled(
                format!(
                    " {} {} {}",
                    if selected { ">" } else { " " },
                    view.index() + 1,
                    view.name(lang)
                ),
                if selected && !no_color {
                    Style::default().fg(GREEN).add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                },
            )));
            lines.push(Line::raw(""));
        }
        lines.push(Line::raw(" Tab / Shift+Tab"));
        lines.push(Line::raw(lang.choose(" alterna telas", " switch panes")));
        frame.render_widget(
            Paragraph::new(lines).block(panel(
                lang.choose("NAVEGAÇÃO", "NAVIGATION"),
                GREEN,
                no_color,
            )),
            columns[0],
        );
        columns[1]
    } else {
        area
    };
    let has_sample = match workspace.active {
        ConnectedView::Ontology => {
            if let Some(state) = &mut workspace.ontology {
                connected::render_at(frame, content, state, address, graph, lang, no_color);
                true
            } else {
                false
            }
        }
        ConnectedView::Memory => {
            if let Some(state) = &workspace.memory {
                connected_memory::render_at(frame, content, state, address, graph, lang, no_color);
                true
            } else {
                false
            }
        }
        ConnectedView::Indexes => {
            if let Some(state) = &mut workspace.indexes {
                connected_indexes::render_at(frame, content, state, address, graph, lang, no_color);
                true
            } else {
                false
            }
        }
        ConnectedView::Graph => {
            if let Some(state) = &mut workspace.graph {
                connected_graph::render_at(frame, content, state, address, graph, lang, no_color);
                true
            } else {
                false
            }
        }
    };
    if !has_sample {
        let sections = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(3),
        ])
        .split(content);
        frame.render_widget(
            Paragraph::new(format!(
                " BRAINWIRE / {} / {} @ {}",
                workspace.active.name(lang),
                terminal_text(graph),
                terminal_text(address)
            ))
            .block(panel("", GREEN, no_color)),
            sections[0],
        );
        let error = workspace.initial_errors[workspace.active.index()]
            .as_deref()
            .unwrap_or("no sample available");
        let text = format!("{}\n\n{}\n\n{}", lang.choose(" Nenhuma amostra disponível.", " No sample available."), terminal_text(&error.chars().take(512).collect::<String>()), lang.choose(" r tenta novamente. Tab ou 1/2/3/4 abre outra tela.\n Esta tela não altera o grafo.", " r retries. Tab or 1/2/3/4 opens another pane.\n This pane does not modify the graph."));
        frame.render_widget(
            Paragraph::new(text).wrap(Wrap { trim: false }).block(panel(
                lang.choose("FALHA AO CARREGAR", "FETCH FAILED"),
                CYAN,
                no_color,
            )),
            sections[1],
        );
    }
    // Reuse the existing footer space so narrow panes retain their explanations.
    let footer = Rect::new(
        content.x,
        content.bottom().saturating_sub(3).max(content.y),
        content.width,
        content.height.min(3),
    );
    let navigation = format!(
        " 1 {}  2 {}  3 {}  4 {} • Tab • r {} • ? {} • q {}",
        ConnectedView::Ontology.name(lang),
        ConnectedView::Memory.name(lang),
        ConnectedView::Indexes.name(lang),
        ConnectedView::Graph.name(lang),
        lang.choose("atualiza", "refresh"),
        lang.choose("ajuda", "help"),
        lang.choose("sai", "quit")
    );
    let controls = if !has_sample {
        lang.choose("SEM AMOSTRA / SOMENTE LEITURA", "NO SAMPLE / READ ONLY")
    } else {
        match workspace.active {
            ConnectedView::Ontology => lang.choose(
                "↑↓ tipo • Enter/F1 detalhes • PgUp/Dn rola",
                "↑↓ type • Enter/F1 inspect • PgUp/Dn scroll",
            ),
            ConnectedView::Memory => lang.choose(
                "POOL DE CONSULTAS / SOMENTE LEITURA",
                "QUERY POOL / READ ONLY",
            ),
            ConnectedView::Indexes => {
                lang.choose("↑↓ índice • PgUp/Dn rola", "↑↓ index • PgUp/Dn scroll")
            }
            ConnectedView::Graph => lang.choose(
                "Até 20 relações; não é o grau total. ↑↓ / PgUp/Dn",
                "Up to 20 relationships; not total degree. ↑↓ / PgUp/Dn",
            ),
        }
    };
    frame.render_widget(
        Paragraph::new(navigation).block(panel(controls, GREEN, no_color)),
        footer,
    );
    if workspace.help {
        let (title, body) = match workspace.active {
            ConnectedView::Ontology => (
                lang.choose("Ontologia conectada", "Connected ontology"),
                lang.choose(
                    "Snapshot real do schema do grafo selecionado. Conexoes sao assinaturas permitidas; nao provam que existam arestas gravadas.\n\n↑↓ escolhe tipo; Enter/F1 abre o inspetor; PgUp/PgDn percorre propriedades e relacoes. Tab ou 1–4 muda de painel; r atualiza.\n\nO catalogo global esta disponivel no inspetor offline. Esta tela conectada mostra o tipo selecionado.",
                    "A real schema snapshot from the selected graph. Connections are allowed signatures; they do not prove stored edge records exist.\n\n↑↓ selects a type; Enter/F1 opens its inspector; PgUp/PgDn scrolls properties and relationships. Tab or 1–4 changes pane; r refreshes.\n\nThe global catalog is available in the offline inspector. This connected view shows the selected type.",
                ),
            ),
            ConnectedView::Memory => (
                lang.choose("Memoria conectada", "Connected memory"),
                lang.choose(
                    "Contadores atuais do pool compartilhado de consultas no servidor. Reservas podem durar ate o fim da transacao; nao sao RSS nem uso total do processo.\n\nTab ou 1–4 muda de painel; r busca uma amostra nova. Esta tela e somente leitura.\n\nA TUI conectada consulta o servidor sem abrir os arquivos RocksDB localmente.",
                    "Current counters from the server's shared query pool. Reservations can last until transaction end; these are not RSS or total process usage.\n\nTab or 1–4 changes pane; r fetches a new sample. This pane is read-only.\n\nThe connected TUI queries the server without opening local RocksDB files.",
                ),
            ),
            ConnectedView::Indexes => (
                lang.choose("Indices conectados", "Connected indexes"),
                lang.choose(
                    "Declarado significa que o schema pede o indice. Pronto significa que a construcao terminou. Enquanto pendente, a consulta pode usar scan.\n\n↑↓ ou PgUp/PgDn seleciona indice; Tab ou 1–4 muda de painel; r atualiza. Esta tela e somente leitura.",
                    "Declared means the schema requests the index. Ready means construction finished. While pending, a query may use a scan.\n\n↑↓ or PgUp/PgDn selects an index; Tab or 1–4 changes pane; r refreshes. This pane is read-only.",
                ),
            ),
            ConnectedView::Graph => (
                lang.choose("Grafo conectado", "Connected graph"),
                lang.choose(
                    "Explora uma amostra limitada de relacoes reais a partir do nodo selecionado. O limite da amostra nao e o grau total do nodo.\n\n↑↓ seleciona resultado; campos de configuracao identificam nodo e relacao. Tab ou 1–4 muda de painel; r atualiza. Nenhuma escrita e feita.",
                    "Explores a bounded sample of real relationships from the selected node. The sample limit is not the node's total degree.\n\n↑↓ selects a result; configuration fields identify the node and relationship. Tab or 1–4 changes pane; r refreshes. No writes are performed.",
                ),
            ),
        };
        let width = content.width.saturating_sub(4).clamp(1, 88);
        let height = content.height.saturating_sub(2).clamp(1, 24);
        let popup = Rect::new(
            content.x + content.width.saturating_sub(width) / 2,
            content.y + content.height.saturating_sub(height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(format!(
                "{body}\n\n{}",
                lang.choose("? ou Esc fecha a ajuda.", "? or Esc closes help.")
            ))
            .wrap(Wrap { trim: false })
            .block(panel(title, GREEN, no_color)),
            popup,
        );
    }
}

/// One terminal loop with lazy, synchronous read-only pane refreshes.
pub fn run_connected_workspace(
    options: &Options,
    address: &str,
    graph: &str,
    mut fetch: impl FnMut(ConnectedView) -> Result<ConnectedSnapshot, String>,
) -> Result<(), String> {
    let lang = Language::parse(&options.lang)?;
    if options.view != "workspace" {
        return Err("connected workspace requires --view workspace".into());
    }
    if options.snapshot
        && (!(80..=240).contains(&options.width) || !(24..=80).contains(&options.height))
    {
        return Err("snapshot size must be 80..240 columns and 24..80 rows".into());
    }
    if !options.snapshot && (!io::stdin().is_terminal() || !io::stdout().is_terminal()) {
        return Err("interactive shell requires a terminal; use --snapshot for plain text".into());
    }
    let no_color = std::env::var_os("NO_COLOR").is_some();
    let mut workspace = Workspace::new();
    workspace.refresh(&mut fetch);
    if options.snapshot {
        if let Some(error) = &workspace.initial_errors[ConnectedView::Ontology.index()] {
            return Err(error.clone());
        }
        let mut terminal = Terminal::new(TestBackend::new(options.width, options.height))
            .map_err(|e| e.to_string())?;
        terminal
            .draw(|f| render(f, &mut workspace, address, graph, lang, no_color))
            .map_err(|e| e.to_string())?;
        let buffer = terminal.backend().buffer();
        for y in 0..options.height {
            let row: String = (0..options.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect();
            println!("{}", row.trim_end());
        }
        return Ok(());
    }
    let _guard = TerminalGuard::enter()?;
    let mut terminal =
        Terminal::new(CrosstermBackend::new(io::stdout())).map_err(|e| e.to_string())?;
    loop {
        terminal
            .draw(|f| render(f, &mut workspace, address, graph, lang, no_color))
            .map_err(|e| e.to_string())?;
        if event::poll(Duration::from_secs(1)).map_err(|e| e.to_string())? {
            if let Event::Key(key) = event::read().map_err(|e| e.to_string())? {
                if workspace.handle(key, &mut fetch) {
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn fetch(view: ConnectedView) -> Result<ConnectedSnapshot, String> {
        Ok(match view {
            ConnectedView::Ontology => ConnectedSnapshot::Ontology(OntologySnapshot {
                ontology: OntologySummary {
                    nodes: ["Ticket", "Comment"]
                        .map(|label| OntologyNode {
                            label: label.into(),
                            ..Default::default()
                        })
                        .to_vec(),
                    edges: vec![],
                },
                version: 3,
                indexes_ready: true,
            }),
            ConnectedView::Memory => ConnectedSnapshot::Memory(MemorySnapshot {
                pool_limit_bytes: Some(64 << 20),
                pool_reserved_bytes: Some(1536),
                pool_peak_bytes: Some(2 << 30),
                pool_rejections: Some(7),
            }),
            ConnectedView::Graph => ConnectedSnapshot::Graph(GraphSnapshot {
                node_label: "Ticket".into(),
                entity_id: "ticket-1".into(),
                edge_type: "HAS_COMMENT".into(),
                target_label: "TicketComment".into(),
                found: true,
                neighbors: vec!["comment-1".into()],
                has_more: false,
            }),
            ConnectedView::Indexes => ConnectedSnapshot::Indexes(IndexesSnapshot {
                indexes: (0..20)
                    .map(|i| IndexSummary {
                        label: format!("Type{i}"),
                        property: "name".into(),
                        state: "scalar_equality_ready".into(),
                    })
                    .collect(),
            }),
        })
    }
    fn text(
        workspace: &mut Workspace,
        lang: Language,
        width: u16,
        height: u16,
        no_color: bool,
    ) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    workspace,
                    "server\x1b[31m",
                    "graph\n",
                    lang,
                    no_color,
                )
            })
            .unwrap();
        let cells = &terminal.backend().buffer().content;
        if no_color {
            assert!(cells
                .iter()
                .all(|cell| cell.fg == Color::Reset && cell.bg == Color::Reset));
        }
        cells.iter().map(|cell| cell.symbol()).collect()
    }
    #[test]
    fn navigation_fetches_lazily_once_and_preserves_each_selection() {
        let mut calls = Vec::new();
        let mut counted = |view| {
            calls.push(view);
            fetch(view)
        };
        let mut workspace = Workspace::new();
        workspace.refresh(&mut counted);
        workspace.handle(key(KeyCode::Down), &mut counted);
        workspace.handle(key(KeyCode::Enter), &mut counted);
        workspace.handle(key(KeyCode::Char('3')), &mut counted);
        workspace.handle(key(KeyCode::PageDown), &mut counted);
        let index_age = workspace.indexes.as_ref().unwrap().sampled;
        workspace.handle(key(KeyCode::BackTab), &mut counted);
        workspace.handle(key(KeyCode::BackTab), &mut counted);
        assert_eq!(workspace.active, ConnectedView::Ontology);
        assert_eq!(workspace.ontology.as_ref().unwrap().selected, 1);
        assert!(workspace.ontology.as_ref().unwrap().inspect);
        workspace.handle(key(KeyCode::Char('3')), &mut counted);
        assert_eq!(workspace.indexes.as_ref().unwrap().selected, 5);
        assert_eq!(workspace.indexes.as_ref().unwrap().sampled, index_age);
        assert_eq!(
            calls,
            [
                ConnectedView::Ontology,
                ConnectedView::Indexes,
                ConnectedView::Memory
            ]
        );
        workspace.handle(KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT), &mut fetch);
        assert_eq!(workspace.active, ConnectedView::Memory);
    }

    #[test]
    fn contextual_help_is_available_on_every_connected_pane_and_does_not_move_selection() {
        for lang in [Language::En, Language::PtBr] {
            for (width, height) in [(80, 24), (120, 30), (160, 48)] {
                for (view, meaning_en, meaning_pt) in [
                    (ConnectedView::Ontology, "allowed", "permitidas"),
                    (ConnectedView::Memory, "not RSS", "nao sao RSS"),
                    (
                        ConnectedView::Indexes,
                        "Declared means",
                        "Declarado significa",
                    ),
                    (ConnectedView::Graph, "bounded sample", "amostra limitada"),
                ] {
                    let mut workspace = Workspace::new();
                    let mut fetcher = fetch;
                    workspace.refresh(&mut fetcher);
                    workspace.select(view, &mut fetcher);
                    workspace.handle(key(KeyCode::Char('?')), &mut fetcher);
                    assert!(workspace.help);
                    let actual = if lang == Language::En {
                        meaning_en
                    } else {
                        meaning_pt
                    };
                    let output = text(&mut workspace, lang, width, height, true);
                    assert!(output.contains(actual), "{view:?}: {output}");
                    assert!(output.contains(if lang == Language::En {
                        "? or Esc closes help"
                    } else {
                        "? ou Esc fecha a ajuda"
                    }));
                    workspace.handle(key(KeyCode::Down), &mut fetcher);
                    assert!(workspace.help);
                    workspace.handle(key(KeyCode::Esc), &mut fetcher);
                    assert!(!workspace.help);
                }
            }
        }
    }
    #[test]
    fn forbidden_memory_keeps_workspace_open_and_other_panes_available() {
        let mut workspace = Workspace::new();
        let mut restricted = |view| {
            if view == ConnectedView::Memory {
                Err("Neo.ClientError.Security.Forbidden\x1b[31m".into())
            } else {
                fetch(view)
            }
        };
        workspace.refresh(&mut restricted);
        assert!(!workspace.handle(key(KeyCode::Tab), &mut restricted));
        assert!(workspace.memory.is_none());
        let output = text(&mut workspace, Language::En, 80, 24, true);
        assert!(output.contains("FETCH FAILED"));
        assert!(output.contains("Forbidden"));
        assert!(!output.contains('\x1b'));
        assert!(output.contains("No sample available"));
        assert!(!output.contains("unknown"));
        workspace.handle(key(KeyCode::Tab), &mut restricted);
        assert!(workspace.indexes.is_some());
        workspace.handle(key(KeyCode::Char('1')), &mut restricted);
        assert!(text(&mut workspace, Language::En, 80, 24, true).contains("Ticket"));
    }
    #[test]
    fn refresh_failure_retains_real_sample_age_and_recovers() {
        let mut workspace = Workspace::new();
        workspace.select(ConnectedView::Memory, &mut fetch);
        let old = std::time::Instant::now() - Duration::from_secs(42);
        workspace.memory.as_mut().unwrap().sampled = old;
        workspace.handle(key(KeyCode::Char('r')), &mut |_| Err("offline".into()));
        assert_eq!(workspace.memory.as_ref().unwrap().sampled, old);
        let stale = text(&mut workspace, Language::En, 80, 24, true);
        for expected in ["STALE", "42s", "64.00 MiB", "REJECTIONS  7", "RSS", "cache"] {
            assert!(stale.contains(expected), "{expected}: {stale}");
        }
        workspace.handle(key(KeyCode::Char('r')), &mut fetch);
        assert!(workspace.memory.as_ref().unwrap().error.is_none());
        assert!(workspace.memory.as_ref().unwrap().sampled > old);
        assert!(workspace.initial_errors[1].is_none());
    }
    #[test]
    fn all_panes_render_at_beta_sizes_in_both_languages_without_clipping_core_data() {
        for lang in [Language::En, Language::PtBr] {
            for (width, height) in [(80, 24), (120, 30)] {
                for view in [
                    ConnectedView::Ontology,
                    ConnectedView::Memory,
                    ConnectedView::Indexes,
                ] {
                    let mut workspace = Workspace::new();
                    workspace.select(view, &mut fetch);
                    for stale in [false, true] {
                        if stale {
                            workspace.refresh(&mut |_| Err("offline".into()));
                        }
                        let output = text(&mut workspace, lang, width, height, true);
                        for expected in ["BRAINWIRE", "Tab", "1", "2", "3"] {
                            assert!(output.contains(expected));
                        }
                        assert!(!output.contains('\x1b'));
                        if width == 120 {
                            assert!(output.contains(lang.choose("NAVEGAÇÃO", "NAVIGATION")));
                        }
                        match view {
                            ConnectedView::Ontology => assert!(output.contains("Ticket")),
                            ConnectedView::Graph => assert!(output.contains("ticket-1")),
                            ConnectedView::Memory => {
                                for expected in [
                                    "64.00 MiB (67108864 B)",
                                    "1.50 KiB (1536 B)",
                                    "2.00 GiB (2147483648 B)",
                                    lang.choose("REJEIÇÕES  7", "REJECTIONS  7"),
                                    "RSS",
                                    "cache",
                                    lang.choose("consulta desta amostra", "sampling query"),
                                ] {
                                    assert!(
                                        output.contains(expected),
                                        "{width}x{height} {expected}: {output}"
                                    );
                                }
                            }
                            ConnectedView::Indexes => {
                                for expected in [
                                    "Type0",
                                    "name",
                                    lang.choose("PRONTO", "READY"),
                                    lang.choose("valores complexos", "complex values"),
                                ] {
                                    assert!(output.contains(expected), "{expected}: {output}");
                                }
                            }
                        }
                    }
                }
            }
        }
        let mut workspace = Workspace::new();
        workspace.select(ConnectedView::Memory, &mut fetch);
        for (width, height) in [(1, 1), (40, 10)] {
            let _ = text(&mut workspace, Language::En, width, height, true);
        }
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    &mut workspace,
                    "server",
                    "graph",
                    Language::En,
                    false,
                )
            })
            .unwrap();
        assert!(terminal
            .backend()
            .buffer()
            .content
            .iter()
            .any(|c| c.fg == GREEN));
        assert!(terminal
            .backend()
            .buffer()
            .content
            .iter()
            .any(|c| c.fg == CYAN));
    }
    #[test]
    fn wrong_snapshot_is_error_and_initial_failure_can_be_retried() {
        let mut workspace = Workspace::new();
        workspace.refresh(&mut |_| Ok(ConnectedSnapshot::Memory(MemorySnapshot::default())));
        assert!(workspace.ontology.is_none());
        assert!(workspace.initial_errors[0]
            .as_ref()
            .unwrap()
            .contains("different pane"));
        workspace.handle(key(KeyCode::Char('r')), &mut fetch);
        assert!(workspace.ontology.is_some());
        assert!(workspace.initial_errors[0].is_none());
        let invalid = Options {
            demo: false,
            lang: "en".into(),
            no_anim: true,
            snapshot: true,
            view: "workspace".into(),
            width: 79,
            height: 24,
        };
        assert!(
            run_connected_workspace(&invalid, "server", "graph", |_| panic!(
                "validation precedes fetch"
            ))
            .unwrap_err()
            .contains("snapshot size")
        );
    }
    #[test]
    fn fourth_pane_is_lazy_keeps_selection_and_permission_failure_is_navigable() {
        let mut calls = Vec::new();
        let mut counted = |view| {
            calls.push(view);
            fetch(view)
        };
        let mut workspace = Workspace::new();
        workspace.refresh(&mut counted);
        workspace.handle(key(KeyCode::Char('4')), &mut counted);
        assert_eq!(workspace.active, ConnectedView::Graph);
        workspace.handle(key(KeyCode::Tab), &mut counted);
        assert_eq!(workspace.active, ConnectedView::Ontology);
        workspace.handle(key(KeyCode::BackTab), &mut counted);
        assert_eq!(workspace.active, ConnectedView::Graph);
        assert_eq!(calls, [ConnectedView::Ontology, ConnectedView::Graph]);
        for lang in [Language::En, Language::PtBr] {
            for (w, h) in [(80, 24), (120, 30)] {
                let output = text(&mut workspace, lang, w, h, true);
                for expected in [
                    "ticket-1",
                    "comment-1",
                    "HAS_COMMENT",
                    "4",
                    lang.choose("não é o grau total", "not total degree"),
                ] {
                    assert!(output.contains(expected), "{expected}: {output}");
                }
            }
        }
        let mut denied = Workspace::new();
        denied.select(ConnectedView::Graph, &mut |_| Err("Forbidden".into()));
        assert!(text(&mut denied, Language::En, 80, 24, true).contains("FETCH FAILED"));
        denied.handle(key(KeyCode::Tab), &mut fetch);
        assert_eq!(denied.active, ConnectedView::Ontology);
        assert!(denied.ontology.is_some());
    }
    #[test]
    fn snapshot_initial_failure_returns_error_for_automation() {
        let options = Options {
            demo: false,
            lang: "en".into(),
            no_anim: true,
            snapshot: true,
            view: "workspace".into(),
            width: 80,
            height: 24,
        };
        let mut calls = Vec::new();
        let error = run_connected_workspace(&options, "server", "graph", |view| {
            calls.push(view);
            Err("Neo.ClientError.Security.Forbidden".into())
        })
        .unwrap_err();
        assert_eq!(error, "Neo.ClientError.Security.Forbidden");
        assert_eq!(calls, [ConnectedView::Ontology]);
    }
}
