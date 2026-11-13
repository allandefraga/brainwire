//! Shared read-only workspace; each pane keeps its own sample and selection.
use crate::overview::{panel, terminal_text};
use crate::*;
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::Style,
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
    fn purpose(self, lang: Language) -> &'static str {
        match self {
            Self::Ontology => lang.choose("Tipos e ligações", "Types & links"),
            Self::Memory => lang.choose("Pool de consultas", "Query budget"),
            Self::Indexes => lang.choose("Estado de índices", "Lookup readiness"),
            Self::Graph => lang.choose("Relações reais", "Stored links"),
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

/// One outcome in the connected principal's current standalone graph history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestOutcome {
    pub committed: bool,
    pub request_id: String,
    pub incarnation: String,
    pub sequence: Option<String>,
}
#[derive(Default)]
struct RequestInspector {
    input: String,
    pending: bool,
    outcome: Option<RequestOutcome>,
    error: Option<String>,
}
impl RequestInspector {
    fn handle(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Esc => return true,
            KeyCode::Backspace => {
                self.input.pop();
                self.outcome = None;
                self.error = None;
            }
            KeyCode::Char(c) if c.is_ascii_graphic() && self.input.len() < 128 => {
                self.input.push(c);
                self.outcome = None;
                self.error = None;
            }
            KeyCode::Enter if !self.input.is_empty() => self.pending = true,
            KeyCode::Enter => {
                self.error = Some("Enter 1..128 printable ASCII characters without spaces".into())
            }
            _ => {}
        }
        false
    }
    fn inspect(&mut self, lookup: &mut impl FnMut(&str) -> Result<RequestOutcome, String>) {
        if !self.pending {
            return;
        }
        self.pending = false;
        self.outcome = None;
        self.error = None;
        match lookup(&self.input) {
            Ok(outcome) if outcome.request_id == self.input => self.outcome = Some(outcome),
            Ok(_) => self.error = Some("server returned a different request ID".into()),
            Err(error) => self.error = Some(error.chars().take(512).collect()),
        }
    }
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
    request: Option<RequestInspector>,
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
            request: None,
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
        if let Some(request) = &mut self.request {
            if request.handle(key.code) {
                self.request = None;
            }
            return false;
        }
        if key.code == KeyCode::Char('q') {
            return true;
        }
        if self.help {
            if matches!(key.code, KeyCode::Char('?') | KeyCode::F(1) | KeyCode::Esc) {
                self.help = false;
            }
            return false;
        }
        match key.code {
            KeyCode::Char('?') => self.help = true,
            KeyCode::F(1) if self.active != ConnectedView::Ontology => self.help = true,
            KeyCode::Char('s') => self.request = Some(RequestInspector::default()),
            KeyCode::Right => {
                self.select(ConnectedView::from_index(self.active.index() + 1), fetch)
            }
            KeyCode::Left => self.select(ConnectedView::from_index(self.active.index() + 3), fetch),
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
                    }
                }
                ConnectedView::Indexes => {
                    if let Some(state) = &mut self.indexes {
                        match key.code {
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
                    }
                }
                ConnectedView::Graph => {
                    if let Some(state) = &mut self.graph {
                        state.handle(key.code);
                    }
                }
                ConnectedView::Memory => {}
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
    // Give the graph canvas enough room on a common 120-column terminal.
    let show_navigation =
        area.width >= 120 && (workspace.active != ConnectedView::Graph || area.width >= 144);
    let content = if show_navigation {
        let columns = Layout::horizontal([Constraint::Length(22), Constraint::Min(0)]).split(area);
        let mut lines = vec![
            Line::from(Span::styled(
                lang.choose(" CONECTADO", " CONNECTED"),
                if no_color {
                    Style::default()
                } else {
                    crate::overview::text_style(MUTED, no_color)
                },
            )),
            Line::raw(lang.choose(" Somente leitura", " Read only")),
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
                if selected {
                    selection_style(no_color)
                } else {
                    crate::overview::text_style(PALE, no_color)
                },
            )));
            lines.push(Line::from(Span::styled(
                format!("   {}", view.purpose(lang)),
                if no_color {
                    Style::default()
                } else {
                    crate::overview::text_style(MUTED, no_color)
                },
            )));
            lines.push(Line::raw(""));
        }
        lines.push(Line::raw(
            lang.choose(" Tab  próxima tela", " Tab  next view"),
        ));
        lines.push(Line::raw(
            lang.choose(" ?    guia rápido", " ?    quick guide"),
        ));
        lines.push(Line::raw(
            lang.choose(" s    status de pedido", " s    request status"),
        ));
        lines.push(Line::raw(lang.choose(" q    sair", " q    quit")));
        frame.render_widget(
            Paragraph::new(lines).block(panel(
                lang.choose("NAVEGAÇÃO", "NAVIGATION"),
                ACCENT,
                no_color,
            )),
            columns[0],
        );
        columns[1]
    } else {
        area
    };
    // The named view bar needs two rows even when the sidebar is collapsed.
    let pane_area = Rect::new(
        content.x,
        content.y,
        content.width,
        content.height.saturating_sub(1),
    );
    let has_sample = match workspace.active {
        ConnectedView::Ontology => {
            if let Some(state) = &mut workspace.ontology {
                connected::render_at(frame, pane_area, state, address, graph, lang, no_color);
                true
            } else {
                false
            }
        }
        ConnectedView::Memory => {
            if let Some(state) = &workspace.memory {
                connected_memory::render_at(
                    frame, pane_area, state, address, graph, lang, no_color,
                );
                true
            } else {
                false
            }
        }
        ConnectedView::Indexes => {
            if let Some(state) = &mut workspace.indexes {
                connected_indexes::render_at(
                    frame, pane_area, state, address, graph, lang, no_color,
                );
                true
            } else {
                false
            }
        }
        ConnectedView::Graph => {
            if let Some(state) = &mut workspace.graph {
                connected_graph::render_at(frame, pane_area, state, address, graph, lang, no_color);
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
        .split(pane_area);
        frame.render_widget(
            Paragraph::new(format!(
                " BRAINWIRE / {} / {} @ {}",
                workspace.active.name(lang),
                terminal_text(graph),
                terminal_text(address)
            ))
            .block(panel("", ACCENT, no_color)),
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
    // Keep view names and actions visible when the sidebar is collapsed.
    let footer = Rect::new(
        content.x,
        content.bottom().saturating_sub(4).max(content.y),
        content.width,
        content.height.min(4),
    );
    let navigation = if workspace.active == ConnectedView::Ontology {
        lang.choose(
            " Tab/←→ telas  r atualiza  s pedido  ? ajuda  q sai",
            " Tab/←→ views  r refresh  s request  ? help  q quit",
        )
    } else {
        lang.choose(
            " Tab/←→ telas  r atualiza  s pedido  ? ajuda / F1  q sai",
            " Tab/←→ views  r refresh  s request  ? help / F1  q quit",
        )
    };
    let controls = if !has_sample {
        lang.choose("SEM AMOSTRA / SOMENTE LEITURA", "NO SAMPLE / READ ONLY")
    } else {
        match workspace.active {
            ConnectedView::Ontology => lang.choose(
                "↑↓ escolhe tipo • Enter detalhes • Tab próxima tela",
                "↑↓ select type • Enter details • Tab next view",
            ),
            ConnectedView::Memory => lang.choose(
                "MEMÓRIA DE CONSULTAS • somente leitura • Tab próxima tela",
                "QUERY MEMORY • read only • Tab next view",
            ),
            ConnectedView::Indexes => lang.choose(
                "↑↓ escolhe índice • Tab próxima tela",
                "↑↓ select index • Tab next view",
            ),
            ConnectedView::Graph => lang.choose(
                "↑↓ relação • Home/End início/fim • Tab próxima tela",
                "↑↓ relationship • Home/End first/last • Tab next view",
            ),
        }
    };
    let views = [
        ConnectedView::Ontology,
        ConnectedView::Memory,
        ConnectedView::Indexes,
        ConnectedView::Graph,
    ]
    .into_iter()
    .map(|view| {
        let selected = workspace.active == view;
        Span::styled(
            format!(
                " {}{} {}{} ",
                if selected { "[" } else { " " },
                view.index() + 1,
                view.name(lang),
                if selected { "]" } else { " " },
            ),
            if selected {
                selection_style(no_color)
            } else {
                crate::overview::text_style(MUTED, no_color)
            },
        )
    })
    .collect::<Vec<_>>();
    frame.render_widget(Clear, footer);
    frame.render_widget(
        Paragraph::new(vec![Line::from(views), Line::raw(navigation)])
            .block(panel(controls, ACCENT, no_color)),
        footer,
    );
    if let Some(request) = &workspace.request {
        let width = content.width.saturating_sub(2).clamp(1, 88);
        let height = content.height.saturating_sub(2).clamp(1, 21);
        let popup = Rect::new(
            content.x + content.width.saturating_sub(width) / 2,
            content.y + content.height.saturating_sub(height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, popup);
        let border = panel(
            lang.choose(
                "INSPECIONAR PEDIDO / EXPERIMENTAL",
                "INSPECT REQUEST / EXPERIMENTAL",
            ),
            ACCENT,
            no_color,
        );
        let inner = border.inner(popup);
        frame.render_widget(border, popup);
        let sections = Layout::vertical([
            Constraint::Length(2),
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(6),
        ])
        .split(inner);
        frame.render_widget(
            Paragraph::new(format!(
                "{} {}\n{}",
                lang.choose("Grafo:", "Graph:"),
                terminal_text(graph),
                lang.choose(
                    "Ledger standalone experimental; mesmo principal.",
                    "Experimental standalone ledger; same principal."
                )
            ))
            .wrap(Wrap { trim: false }),
            sections[0],
        );
        frame.render_widget(
            Paragraph::new(format!(
                "ID: {}\n{}",
                terminal_text(&request.input),
                lang.choose(
                    "1..128 ASCII visíveis, sem espaços.",
                    "1..128 printable ASCII characters, no spaces."
                )
            ))
            .wrap(Wrap { trim: false }),
            sections[1],
        );
        let mut result = String::new();
        if let Some(outcome) = &request.outcome {
            result.push_str(&format!(
                "{}: {}\nIncarnation: {}\nSequence: {}",
                lang.choose("Estado", "Status"),
                if outcome.committed {
                    "committed"
                } else {
                    "not_recorded"
                },
                terminal_text(&outcome.incarnation),
                outcome
                    .sequence
                    .as_deref()
                    .map(terminal_text)
                    .unwrap_or_else(|| "—".into())
            ));
        }
        if let Some(error) = &request.error {
            result.push_str(&terminal_text(error));
        }
        frame.render_widget(
            Paragraph::new(result).wrap(Wrap { trim: false }),
            sections[2],
        );
        frame.render_widget(Paragraph::new(lang.choose(
            "not_recorded é inconclusivo; não repita escrita.\ncommitted vale apenas para o histórico atual.\nQuorum: cluster request --id <effective-key>.\n\nEnter consulta uma vez; Esc fecha. Somente leitura.",
            "not_recorded is inconclusive; do not replay a write.\ncommitted refers only to current store history.\nQuorum: cluster request --id <effective-key>.\n\nEnter looks up once; Esc closes. Read only.")).wrap(Wrap { trim: false }), sections[3]);
    }
    if workspace.help {
        let (title, body) = match workspace.active {
            ConnectedView::Ontology => (
                lang.choose("Guia rápido / Ontologia", "Quick guide / Ontology"),
                lang.choose(
                    "Conexões são assinaturas permitidas pelo esquema; não provam que existam arestas gravadas.\n\n↑↓ escolhe tipo; Enter/F1 abre detalhes; PgUp/PgDn rola.",
                    "Connections are allowed schema links; they do not prove stored edges exist.\n\n↑↓ selects a type; Enter/F1 opens details; PgUp/PgDn scrolls.",
                ),
            ),
            ConnectedView::Memory => (
                lang.choose("Guia rápido / Memória", "Quick guide / Memory"),
                lang.choose(
                    "Reservas do pool compartilhado de consultas no servidor. Podem durar até o fim da transação; não são RSS nem o uso total do processo.\n\nr busca uma amostra nova.",
                    "The server's shared query reservations can last until transaction end. They are not RSS or total process usage.\n\nr fetches a new sample.",
                ),
            ),
            ConnectedView::Indexes => (
                lang.choose("Guia rápido / Índices", "Quick guide / Indexes"),
                lang.choose(
                    "Declarado significa pedido pelo esquema. Pronto significa que a construção terminou. Um índice pendente pode exigir uma varredura dos dados.\n\n↑↓ escolhe índice; PgUp/PgDn percorre a lista.",
                    "Declared means requested by the schema. Ready means the build finished. A pending index may still require a scan.\n\n↑↓ selects an index; PgUp/PgDn moves through the list.",
                ),
            ),
            ConnectedView::Graph => (
                lang.choose("Guia rápido / Grafo", "Quick guide / Graph"),
                lang.choose(
                    "Uma amostra limitada de relações reais do nó configurado. O limite de 20 relações não é o grau total do nó. IDs de destino podem repetir.\n\n↑↓ escolhe uma relação; PgUp/PgDn percorre a lista; Home/End vai ao início/fim.",
                    "A bounded sample of real relationships for the configured node. The 20-relationship sample is not the node's total degree. Target IDs can repeat.\n\n↑↓ selects a relationship; PgUp/PgDn moves through the list; Home/End jumps to first/last.",
                ),
            ),
        };
        let guide = lang.choose(
            "1 Ontologia  Entenda os tipos e ligações permitidas\n2 Memória    Leia o orçamento de consultas\n3 Índices    Confira se uma busca está pronta\n4 Grafo      Veja as relações gravadas\n\nTab / Shift+Tab ou ←→ alterna telas. r atualiza.\ns inspeciona um pedido standalone. Esc fecha; q sai fora do inspetor.\nEsta TUI é somente leitura.\n\nNESTA TELA\n",
            "1 Ontology  Understand types and allowed links\n2 Memory    Read the query memory budget\n3 Indexes   Check whether a lookup is ready\n4 Graph     Inspect stored connections\n\nTab / Shift+Tab or ←→ switches views. r refreshes.\ns inspects one standalone request. Esc closes; q quits outside inspector.\nThis workspace is read only.\n\nTHIS VIEW\n",
        );
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
                "{guide}{body}\n\n{}",
                lang.choose("? ou Esc fecha a ajuda.", "? or Esc closes help.")
            ))
            .wrap(Wrap { trim: false })
            .block(panel(title, ACCENT, no_color)),
            popup,
        );
    }
}

/// One terminal loop with lazy, synchronous read-only pane refreshes.
pub fn run_connected_workspace(
    options: &Options,
    address: &str,
    graph: &str,
    fetch: impl FnMut(ConnectedView) -> Result<ConnectedSnapshot, String>,
) -> Result<(), String> {
    run_connected_workspace_with_requests(options, address, graph, fetch, |_| {
        Err("Request inspection is unavailable in this client".into())
    })
}

/// The inspector issues one authenticated, read-only standalone outcome lookup.
pub fn run_connected_workspace_with_requests(
    options: &Options,
    address: &str,
    graph: &str,
    mut fetch: impl FnMut(ConnectedView) -> Result<ConnectedSnapshot, String>,
    mut lookup: impl FnMut(&str) -> Result<RequestOutcome, String>,
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
                if let Some(request) = &mut workspace.request {
                    request.inspect(&mut lookup);
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
                        entity_type: "node".into(),
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
    fn horizontal_arrows_switch_views_and_keep_inspector_input_local() {
        let mut workspace = Workspace::new();
        workspace.refresh(&mut fetch);
        workspace.handle(key(KeyCode::Right), &mut fetch);
        assert_eq!(workspace.active, ConnectedView::Memory);
        workspace.handle(key(KeyCode::Left), &mut fetch);
        assert_eq!(workspace.active, ConnectedView::Ontology);
        workspace.handle(key(KeyCode::Left), &mut fetch);
        assert_eq!(workspace.active, ConnectedView::Graph);
        workspace.handle(key(KeyCode::Char('s')), &mut fetch);
        workspace.handle(key(KeyCode::Left), &mut fetch);
        assert_eq!(workspace.active, ConnectedView::Graph);
        assert!(workspace.request.is_some());
    }

    #[test]
    fn narrow_workspace_keeps_named_views_visible_without_opening_help() {
        for (lang, pages) in [
            (
                Language::En,
                ["1 Ontology", "2 Memory", "3 Indexes", "4 Graph"],
            ),
            (
                Language::PtBr,
                ["1 Ontologia", "2 Memória", "3 Índices", "4 Grafo"],
            ),
        ] {
            for view in [
                ConnectedView::Ontology,
                ConnectedView::Memory,
                ConnectedView::Indexes,
                ConnectedView::Graph,
            ] {
                let mut workspace = Workspace::new();
                workspace.select(view, &mut fetch);
                for (width, height) in [(80, 24), (120, 36)] {
                    let output = text(&mut workspace, lang, width, height, true);
                    for page in pages {
                        assert!(output.contains(page), "missing {page}: {output}");
                    }
                    assert!(output.contains("Tab/←→"));
                }
            }
        }
    }

    #[test]
    fn f1_explains_non_ontology_panes_without_changing_samples() {
        for view in [
            ConnectedView::Memory,
            ConnectedView::Indexes,
            ConnectedView::Graph,
        ] {
            let mut workspace = Workspace::new();
            workspace.select(view, &mut fetch);
            workspace.handle(key(KeyCode::F(1)), &mut fetch);
            assert!(workspace.help);
            workspace.handle(key(KeyCode::F(1)), &mut fetch);
            assert!(!workspace.help);
            assert_eq!(workspace.active, view);
        }
    }

    #[test]
    fn contextual_help_is_available_on_every_connected_pane_and_does_not_move_selection() {
        for lang in [Language::En, Language::PtBr] {
            for (width, height) in [(80, 24), (120, 30), (160, 48)] {
                for (view, meaning_en, meaning_pt) in [
                    (ConnectedView::Ontology, "allowed", "permitidas"),
                    (ConnectedView::Memory, "RSS", "RSS"),
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
    fn escape_closes_details_and_help_while_only_q_quits_the_workspace() {
        for view in [
            ConnectedView::Ontology,
            ConnectedView::Memory,
            ConnectedView::Indexes,
            ConnectedView::Graph,
        ] {
            let mut workspace = Workspace::new();
            workspace.select(view, &mut fetch);
            assert!(!workspace.handle(key(KeyCode::Esc), &mut fetch));
            if view == ConnectedView::Ontology {
                workspace.handle(key(KeyCode::Enter), &mut fetch);
                assert!(workspace.ontology.as_ref().unwrap().inspect);
                assert!(!workspace.handle(key(KeyCode::Esc), &mut fetch));
                assert!(!workspace.ontology.as_ref().unwrap().inspect);
            }
            workspace.handle(key(KeyCode::Char('?')), &mut fetch);
            assert!(!workspace.handle(key(KeyCode::Esc), &mut fetch));
            assert!(!workspace.help);
            assert!(!workspace.handle(key(KeyCode::Esc), &mut fetch));
            assert!(workspace.handle(key(KeyCode::Char('q')), &mut fetch));
        }
    }
    #[test]
    fn graph_canvas_gets_full_width_on_a_120_column_terminal() {
        let mut workspace = Workspace::new();
        workspace.select(ConnectedView::Graph, &mut fetch);
        let medium = text(&mut workspace, Language::En, 120, 30, true);
        assert!(!medium.contains("NAVIGATION"));
        assert!(medium.contains("ticket-1"));
        assert!(medium.contains("comment-1"));
        let wide = text(&mut workspace, Language::En, 160, 48, true);
        assert!(wide.contains("NAVIGATION"));
        assert!(wide.contains("comment-1"));
    }

    #[test]
    fn narrow_footer_keeps_refresh_help_and_exit_visible() {
        let mut workspace = Workspace::new();
        workspace.refresh(&mut fetch);
        for (lang, controls) in [
            (Language::En, ["r refresh", "? help", "q quit"]),
            (Language::PtBr, ["r atualiza", "? ajuda", "q sai"]),
        ] {
            let output = text(&mut workspace, lang, 80, 24, true);
            for control in controls {
                assert!(output.contains(control), "missing {control}: {output}");
            }
            workspace.help = true;
            let guide = text(&mut workspace, lang, 80, 24, true);
            let pages = if lang == Language::En {
                ["1 Ontology", "2 Memory", "3 Indexes", "4 Graph"]
            } else {
                ["1 Ontologia", "2 Memória", "3 Índices", "4 Grafo"]
            };
            for page in pages {
                assert!(guide.contains(page), "missing {page}: {guide}");
            }
            assert!(guide.contains("Shift+Tab"));
            assert!(workspace.handle(key(KeyCode::Char('q')), &mut fetch));
            workspace.help = false;
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
            .any(|c| c.fg == ACCENT));
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
                for expected in ["ticket-1", "comment-1", "HAS_COMMENT", "4"] {
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

#[cfg(test)]
mod request_tests {
    use super::*;
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    #[test]
    fn request_input_is_bounded_and_lookup_is_explicit() {
        let mut workspace = Workspace::new();
        let mut fetch = |_| panic!("request lookup must not fetch a pane");
        assert!(!workspace.handle(key(KeyCode::Char('s')), &mut fetch));
        for _ in 0..140 {
            workspace.handle(key(KeyCode::Char('q')), &mut fetch);
        }
        workspace.handle(key(KeyCode::Char(' ')), &mut fetch);
        workspace.handle(key(KeyCode::Char('é')), &mut fetch);
        let request = workspace.request.as_mut().unwrap();
        assert_eq!(request.input, "q".repeat(128));
        let mut calls = 0;
        let mut lookup = |seed: &str| {
            calls += 1;
            Ok(RequestOutcome {
                committed: false,
                request_id: seed.into(),
                incarnation: "12345678-1234-4234-8234-123456789abc".into(),
                sequence: None,
            })
        };
        request.inspect(&mut lookup);
        request.handle(KeyCode::Enter);
        request.inspect(&mut lookup);
        request.inspect(&mut lookup);
        assert_eq!(calls, 1);
        assert!(request.outcome.is_some());
        request.handle(KeyCode::Backspace);
        assert!(request.outcome.is_none());
        request.handle(KeyCode::Enter);
        request.inspect(&mut |_| Err("offline".into()));
        assert!(request.outcome.is_none());
        assert_eq!(request.error.as_deref(), Some("offline"));
        workspace.handle(key(KeyCode::Esc), &mut fetch);
        assert!(workspace.request.is_none());
    }
    #[test]
    fn empty_and_mismatched_requests_do_not_display_old_outcomes() {
        let mut request = RequestInspector::default();
        request.handle(KeyCode::Enter);
        assert!(!request.pending);
        assert!(request.error.is_some());
        request.handle(KeyCode::Char('\''));
        request.handle(KeyCode::Char('\\'));
        request.handle(KeyCode::Enter);
        request.inspect(&mut |_| {
            Ok(RequestOutcome {
                committed: true,
                request_id: "other".into(),
                incarnation: "x".into(),
                sequence: Some("1".into()),
            })
        });
        assert!(request.outcome.is_none());
        assert_eq!(
            request.error.as_deref(),
            Some("server returned a different request ID")
        );
    }
    #[test]
    fn safety_and_escape_remain_visible_with_long_input_and_error() {
        for lang in [Language::En, Language::PtBr] {
            let mut workspace = Workspace::new();
            workspace.request = Some(RequestInspector {
                input: "a".repeat(128),
                error: Some("bad\x1b[31m".repeat(64)),
                ..Default::default()
            });
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
            terminal
                .draw(|f| render(f, &mut workspace, "localhost", "test_graph", lang, true))
                .unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect();
            for expected in ["test_graph", "not_recorded", "Quorum:", "Esc", "Enter"] {
                assert!(text.contains(expected), "{expected}: {text}");
            }
            assert!(!text.contains('\x1b'));
        }
    }
}
