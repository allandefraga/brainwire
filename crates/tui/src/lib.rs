//! A bounded, synthetic terminal observatory for Brainwire.
use std::io::{self, IsTerminal, Stdout};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::{CrosstermBackend, TestBackend};
use ratatui::style::Color;
use ratatui::Terminal;
mod connected;
mod connected_graph;
mod connected_indexes;
pub use connected_graph::{run_connected_graph, GraphSnapshot};
mod connected_memory;
mod connected_workspace;
pub use connected_indexes::{run_connected_indexes, IndexSummary, IndexesSnapshot};
pub use connected_memory::{run_connected_memory, MemorySnapshot};
pub use connected_workspace::{run_connected_workspace, ConnectedSnapshot, ConnectedView};
mod ontology;
pub use connected::{run_connected_ontology, OntologySnapshot};
mod overview;
pub use ontology::{OntologyEdge, OntologyNode, OntologySummary};
mod ui;
pub use overview::{run_overview, GraphSummary, StorageFileSummary};
pub use ui::render;

const GREEN: Color = Color::Rgb(0x57, 0xff, 0x48);
const BG: Color = Color::Rgb(0x07, 0x0b, 0x09);
const CYAN: Color = Color::Rgb(0x67, 0xd9, 0xd2);
const PALE: Color = Color::Rgb(0xd9, 0xec, 0xe4);
const EXAMPLES: [&str; 2] = [
    "MATCH (c:Customer) RETURN c.name LIMIT 12",
    "MATCH (c:Customer)-[:WORKS_AT]->(co:Company) RETURN c.name, co.name LIMIT 12",
];

#[derive(Clone, Debug)]
pub struct Options {
    pub demo: bool,
    pub lang: String,
    pub no_anim: bool,
    pub snapshot: bool,
    pub view: String,
    pub width: u16,
    pub height: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Language {
    PtBr,
    En,
}
impl Language {
    fn parse(s: &str) -> Result<Self, String> {
        match s {
            "pt-BR" => Ok(Self::PtBr),
            "en" => Ok(Self::En),
            _ => Err("--lang must be pt-BR or en".into()),
        }
    }
    fn choose(self, pt: &'static str, en: &'static str) -> &'static str {
        if self == Self::PtBr {
            pt
        } else {
            en
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum View {
    Graph,
    Query,
    Memory,
    Disk,
}
impl View {
    fn parse(s: &str) -> Result<Self, String> {
        match s {
            "graph" => Ok(Self::Graph),
            "query" => Ok(Self::Query),
            "memory" => Ok(Self::Memory),
            "disk" => Ok(Self::Disk),
            _ => Err("--view must be graph, query, memory or disk".into()),
        }
    }
    fn index(self) -> usize {
        match self {
            Self::Graph => 0,
            Self::Query => 1,
            Self::Memory => 2,
            Self::Disk => 3,
        }
    }
    fn from_index(i: usize) -> Self {
        [Self::Graph, Self::Query, Self::Memory, Self::Disk][i % 4]
    }
    fn display(self, lang: Language) -> &'static str {
        if lang == Language::En {
            self.name()
        } else {
            match self {
                Self::Graph => "GRAFO",
                Self::Query => "CONSULTA",
                Self::Memory => "MEMÓRIA",
                Self::Disk => "DISCO E ARQUIVOS",
            }
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Graph => "GRAPH",
            Self::Query => "QUERY",
            Self::Memory => "MEMORY",
            Self::Disk => "DISK & FILES",
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Focus {
    Navigation,
    Main,
    Inspector,
}

#[derive(Clone, Debug)]
struct Node {
    id: usize,
    label: &'static str,
    name: String,
}
#[derive(Clone, Debug)]
struct Edge {
    from: usize,
    to: usize,
    kind: &'static str,
}
#[derive(Clone, Debug)]
struct Fixture {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}
impl Fixture {
    fn new() -> Self {
        let customers = [
            "Ana Silva",
            "Bruno Costa",
            "Carla Rocha",
            "Diego Lima",
            "Eva Santos",
            "Felipe Melo",
            "Giulia Alves",
            "Hugo Reis",
            "Iara Nunes",
            "Joao Pinto",
            "Lia Duarte",
            "Mateus Freitas",
        ];
        let companies = [
            "Aurora Labs",
            "Bosque Tech",
            "Ciano Data",
            "Delta Works",
            "Estrela AI",
            "Faro Cloud",
            "Galaxia",
            "Horizonte",
            "Iris Systems",
            "Jade Labs",
            "Kappa",
            "Lumen",
        ];
        let projects = [
            "Atlas", "Brisa", "Cobalto", "Duna", "Eco", "Fenix", "Gama", "Helio", "Ion", "Juno",
            "Kronos", "Lua",
        ];
        let mut nodes = Vec::with_capacity(36);
        for (cluster, (label, names)) in [
            ("Customer", &customers),
            ("Company", &companies),
            ("Project", &projects),
        ]
        .into_iter()
        .enumerate()
        {
            for (i, name) in names.iter().enumerate() {
                nodes.push(Node {
                    id: cluster * 12 + i,
                    label,
                    name: (*name).into(),
                });
            }
        }
        let mut edges = Vec::new();
        for i in 0..12 {
            edges.push(Edge {
                from: i,
                to: 12 + i,
                kind: "WORKS_AT",
            });
            edges.push(Edge {
                from: 12 + i,
                to: 24 + i,
                kind: "BUILDS",
            });
            if i % 3 == 0 {
                edges.push(Edge {
                    from: i,
                    to: 24 + i,
                    kind: "FOLLOWS",
                });
            }
            for cluster in 0..3 {
                edges.push(Edge {
                    from: cluster * 12 + i,
                    to: cluster * 12 + (i + 1) % 12,
                    kind: "LINKED",
                });
            }
        }
        Self { nodes, edges }
    }
}

const FILES: [(&str, &str, &str); 7] = [
    (
        "CURRENT",
        "16 B",
        "Pointer to the active MANIFEST; no graph records.",
    ),
    (
        "MANIFEST-000001",
        "4 KiB",
        "Illustrative version metadata and SST membership.",
    ),
    (
        "000004.log",
        "12 MiB",
        "Illustrative write-ahead log; recent writes before flush.",
    ),
    (
        "000011.sst",
        "64 MiB",
        "Illustrative sorted table containing persisted key/value blocks.",
    ),
    (
        "OPTIONS-000003",
        "3 KiB",
        "Illustrative RocksDB configuration snapshot.",
    ),
    (
        "LOG",
        "8 KiB",
        "Illustrative diagnostic log, not graph data.",
    ),
    (
        "LOCK",
        "0 B",
        "Illustrative process lock marker for exclusive database access.",
    ),
];
const FILES_PT_DESC: [&str; 7] = [
    "Ponteiro para o MANIFEST ativo; não contém registros do grafo.",
    "Metadados ilustrativos de versões e arquivos SST.",
    "WAL ilustrativo; gravações recentes antes do flush.",
    "Tabela ordenada ilustrativa com blocos persistidos de chave/valor.",
    "Cópia ilustrativa da configuração do RocksDB.",
    "Log de diagnóstico ilustrativo, não dados do grafo.",
    "Marcador ilustrativo de bloqueio exclusivo do banco.",
];
const MEM_PT_NAMES: [&str; 5] = ["Contêiner", "Cache", "Memtables", "Consulta", "Outros"];
const MEM_PT_DESC: [&str; 5] = [
    "Escopo físico do contêiner neste cenário sintético. O uso é simulado, não é RSS nem medição do sistema operacional.",
    "Pool de cache configurado do RocksDB. O orçamento de memtables está dentro dos 10 GiB; não é somado novamente.",
    "Orçamento de memtables dentro do cache. O uso de 0.6 GiB já está contabilizado no cache.",
    "Orçamento configurado por consulta. Esta demo não impõe esse limite.",
    "Sobrecarga ilustrativa do motor, alocador e processo. 7.2 cache + 0.8 consulta + 3.5 outros = 11.5 GiB.",
];
const MEM: [(&str, &str, &str); 5] = [
    ("Container", "16 GiB limit / 11.5 GiB simulated used / 4.5 GiB headroom", "Physical container scope in this synthetic scenario. Used is simulated, not RSS or an OS measurement."),
    ("Cache pool", "10 GiB configured / 7.2 GiB simulated used", "Configured RocksDB cache pool. The memtable budget is inside this 10 GiB, not added to it."),
    ("Memtables", "1 GiB budget / 0.6 GiB simulated used", "Memtable budget inside the cache pool. Its 0.6 GiB usage is already counted in cache usage."),
    ("Query", "2 GiB configured / 0.8 GiB simulated used", "Configured per-query memory budget. Enforcement is not implemented in this demo."),
    ("Other", "3.5 GiB simulated used", "Illustrative engine, allocator and process overhead. 7.2 cache + 0.8 query + 3.5 other = 11.5 GiB."),
];

#[derive(Clone, Debug)]
pub struct App {
    pub view: View,
    pub focus: Focus,
    pub help: bool,
    pub editing: bool,
    pub quit: bool,
    pub selected_node: usize,
    pub selected_file: usize,
    pub selected_memory: usize,
    pub example: usize,
    pub query: String,
    pub result: String,
    pub query_scroll: usize,
    pub result_scroll: usize,
    pub inspector_scroll: u16,
    lang: Language,
    fixture: Fixture,
}
impl App {
    pub fn new(lang: Language, view: View) -> Self {
        Self {
            view,
            focus: Focus::Navigation,
            help: false,
            editing: false,
            quit: false,
            selected_node: 0,
            selected_file: if view == View::Disk { 3 } else { 0 },
            selected_memory: 0,
            example: 0,
            query: EXAMPLES[0].into(),
            result: String::new(),
            query_scroll: 0,
            result_scroll: 0,
            inspector_scroll: 0,
            lang,
            fixture: Fixture::new(),
        }
    }
    pub fn handle(&mut self, key: KeyEvent) {
        if self.help {
            if matches!(key.code, KeyCode::F(1) | KeyCode::Esc) {
                self.help = false;
            }
            return;
        }
        if self.editing {
            match key.code {
                KeyCode::Esc | KeyCode::Enter => self.editing = false,
                KeyCode::F(1) => self.help = true,
                KeyCode::F(5) => self.run_query(),
                KeyCode::Backspace => {
                    self.query.pop();
                    self.query_scroll = self.query.chars().count().saturating_sub(48);
                }
                KeyCode::PageUp => self.query_scroll = self.query_scroll.saturating_sub(40),
                KeyCode::PageDown => {
                    self.query_scroll = (self.query_scroll + 40).min(self.query.chars().count())
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.insert(&c.to_string())
                }
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::F(1) => self.help = true,
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Navigation => Focus::Main,
                    Focus::Main => Focus::Inspector,
                    Focus::Inspector => Focus::Navigation,
                }
            }
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('1'..='4') => {
                if let KeyCode::Char(c) = key.code {
                    self.set_view(View::from_index((c as u8 - b'1') as usize));
                }
            }
            KeyCode::PageUp if self.focus == Focus::Inspector => self.move_selection(-4),
            KeyCode::PageDown if self.focus == Focus::Inspector => self.move_selection(4),
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::Left => {
                if self.view == View::Query {
                    self.load_example(0);
                }
            }
            KeyCode::Right => {
                if self.view == View::Query {
                    self.load_example(1);
                }
            }
            KeyCode::Enter => {
                if self.focus == Focus::Inspector {
                    self.focus = Focus::Main;
                } else if self.view == View::Query {
                    self.editing = true;
                    self.focus = Focus::Main;
                } else {
                    self.focus = Focus::Inspector;
                }
            }
            KeyCode::F(5) if self.view == View::Query => self.run_query(),
            _ => {}
        }
    }
    pub fn paste(&mut self, s: &str) {
        if self.editing && !self.help {
            self.insert(s);
        }
    }
    fn insert(&mut self, s: &str) {
        let available = 512usize.saturating_sub(self.query.chars().count());
        self.query
            .extend(s.chars().filter(|c| !c.is_control()).take(available));
        self.query_scroll = self.query.chars().count().saturating_sub(48);
    }
    fn set_view(&mut self, view: View) {
        self.view = view;
        self.inspector_scroll = 0;
        self.focus = Focus::Main;
        self.editing = false;
    }
    fn move_selection(&mut self, delta: isize) {
        if self.focus == Focus::Inspector {
            self.inspector_scroll = self
                .inspector_scroll
                .saturating_add_signed(delta as i16)
                .min(12);
            return;
        }
        self.inspector_scroll = 0;
        if self.focus == Focus::Navigation {
            self.set_view(View::from_index(
                (self.view.index() as isize + delta).rem_euclid(4) as usize,
            ));
            self.focus = Focus::Navigation;
            return;
        }
        let (index, len) = match self.view {
            View::Graph => (&mut self.selected_node, self.fixture.nodes.len()),
            View::Query => {
                let max = self.result.lines().count().saturating_sub(1);
                self.result_scroll = self.result_scroll.saturating_add_signed(delta).min(max);
                return;
            }
            View::Memory => (&mut self.selected_memory, MEM.len()),
            View::Disk => (&mut self.selected_file, FILES.len()),
        };
        *index = (*index as isize + delta).rem_euclid(len as isize) as usize;
    }
    fn load_example(&mut self, index: usize) {
        self.example = index;
        self.query = EXAMPLES[index].into();
        self.result.clear();
        self.query_scroll = 0;
        self.result_scroll = 0;
    }
    pub fn run_query(&mut self) {
        self.result_scroll = 0;
        let normalized = self.query.split_whitespace().collect::<Vec<_>>().join(" ");
        let customer = EXAMPLES[0].split_whitespace().collect::<Vec<_>>().join(" ");
        let works = EXAMPLES[1].split_whitespace().collect::<Vec<_>>().join(" ");
        self.result = if normalized == customer {
            self.fixture.nodes[..12]
                .iter()
                .map(|n| format!("{:02}  {}", n.id, n.name))
                .collect::<Vec<_>>()
                .join("\n")
        } else if normalized == works {
            self.fixture
                .edges
                .iter()
                .filter(|e| e.kind == "WORKS_AT")
                .map(|e| {
                    format!(
                        "{}  →  {}",
                        self.fixture.nodes[e.from].name, self.fixture.nodes[e.to].name
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            self.lang.choose("Consulta fora dos exemplos suportados nesta demo. Use ←/→ para escolher um exemplo; F5 executa somente esses cenários.", "Unsupported demo query. Use ←/→ to choose an example; F5 runs only those guided scenarios.").into()
        };
    }
    pub fn selected_description(&self) -> String {
        match self.view {
            View::Graph => {
                let n = &self.fixture.nodes[self.selected_node];
                let incident = self
                    .fixture
                    .edges
                    .iter()
                    .filter(|e| e.from == n.id || e.to == n.id)
                    .count();
                format!(
                    "{} #{:02} | {} | {} {} | {}",
                    if self.lang == Language::PtBr {
                        match n.label {
                            "Customer" => "Cliente",
                            "Company" => "Empresa",
                            _ => "Projeto",
                        }
                    } else {
                        n.label
                    },
                    n.id,
                    n.name,
                    incident,
                    self.lang.choose("conexões", "connections"),
                    self.lang.choose("dados sintéticos", "synthetic fixture")
                )
            }
            View::Query => self
                .lang
                .choose(
                    "F5 executa somente o exemplo exato; edições livres retornam não suportado.",
                    "F5 runs only the exact example; free edits return unsupported.",
                )
                .into(),
            View::Memory => {
                if self.lang == Language::PtBr {
                    MEM_PT_DESC[self.selected_memory].into()
                } else {
                    MEM[self.selected_memory].2.into()
                }
            }
            View::Disk => {
                if self.lang == Language::PtBr {
                    FILES_PT_DESC[self.selected_file].into()
                } else {
                    FILES[self.selected_file].2.into()
                }
            }
        }
    }
}

struct TerminalGuard {
    alternate: bool,
    paste: bool,
}
impl TerminalGuard {
    fn enter() -> Result<Self, String> {
        enable_raw_mode().map_err(|e| format!("raw mode: {e}"))?;
        let mut guard = Self {
            alternate: false,
            paste: false,
        };
        // A failed write may still have sent the escape sequence to the terminal.
        guard.alternate = true;
        execute!(io::stdout(), EnterAlternateScreen)
            .map_err(|e| format!("alternate screen setup: {e}"))?;
        guard.paste = true;
        execute!(io::stdout(), crossterm::event::EnableBracketedPaste)
            .map_err(|e| format!("bracketed paste setup: {e}"))?;
        Ok(guard)
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if self.paste {
            let _ = execute!(io::stdout(), crossterm::event::DisableBracketedPaste);
        }
        if self.alternate {
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
        }
        let _ = disable_raw_mode();
    }
}

pub fn run(options: Options) -> Result<(), String> {
    let lang = Language::parse(&options.lang)?;
    let view = View::parse(&options.view)?;
    if !options.demo {
        return Err("live backend is not connected yet; use `brainwire shell --demo` for the synthetic observatory".into());
    }
    let no_color = std::env::var_os("NO_COLOR").is_some();
    let app = App::new(lang, view);
    if options.snapshot {
        if !(80..=240).contains(&options.width) || !(24..=80).contains(&options.height) {
            return Err("snapshot size must be 80..240 columns and 24..80 rows".into());
        }
        let backend = TestBackend::new(options.width, options.height);
        let mut terminal = Terminal::new(backend).map_err(|e| e.to_string())?;
        terminal
            .draw(|f| render(f, &app, no_color))
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
        return Err("interactive shell requires a terminal on stdin and stdout; use `brainwire shell --demo --snapshot` for plain text".into());
    }
    let _guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal: Terminal<CrosstermBackend<Stdout>> =
        Terminal::new(backend).map_err(|e| e.to_string())?;
    let mut app = app;
    loop {
        terminal
            .draw(|f| render(f, &app, no_color))
            .map_err(|e| e.to_string())?;
        if app.quit {
            break;
        }
        if event::poll(Duration::from_millis(if options.no_anim {
            250
        } else {
            100
        }))
        .map_err(|e| e.to_string())?
        {
            match event::read().map_err(|e| e.to_string())? {
                Event::Key(key) if key.kind == event::KeyEventKind::Press => app.handle(key),
                Event::Paste(s) => app.paste(&s),
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    #[test]
    fn transitions_and_editor_shortcuts() {
        let mut app = App::new(Language::En, View::Graph);
        app.handle(key(KeyCode::Tab));
        assert_eq!(app.focus, Focus::Main);
        app.handle(key(KeyCode::Char('2')));
        assert_eq!(app.view, View::Query);
        app.handle(key(KeyCode::Enter));
        assert!(app.editing);
        app.query.clear();
        for c in ['q', '1', '4'] {
            app.handle(key(KeyCode::Char(c)));
        }
        assert_eq!(app.query, "q14");
        assert!(!app.quit);
        assert_eq!(app.view, View::Query);
        app.handle(key(KeyCode::Esc));
        assert!(!app.editing);
        app.handle(key(KeyCode::F(1)));
        assert!(app.help);
        app.handle(key(KeyCode::Esc));
        assert!(!app.help);
        app.handle(key(KeyCode::Char('q')));
        assert!(app.quit);
    }
    #[test]
    fn help_is_modal_and_preserves_editor_contents() {
        let mut app = App::new(Language::En, View::Query);
        app.handle(key(KeyCode::Enter));
        let before = app.query.clone();
        app.handle(key(KeyCode::F(1)));
        for code in [
            KeyCode::Char('q'),
            KeyCode::Char('4'),
            KeyCode::F(5),
            KeyCode::Tab,
        ] {
            app.handle(key(code));
        }
        app.paste("unexpected mutation");
        assert!(app.help && app.editing);
        assert!(!app.quit);
        assert_eq!(app.view, View::Query);
        assert_eq!(app.query, before);
        assert!(app.result.is_empty());
        app.handle(key(KeyCode::Esc));
        assert!(!app.help);
        assert!(app.editing);
    }

    #[test]
    fn page_help_and_navigation_are_readable_at_compact_sizes() {
        for (w, h) in [(80, 24), (120, 30), (160, 48)] {
            for lang in [Language::En, Language::PtBr] {
                for view in [View::Graph, View::Query, View::Memory, View::Disk] {
                    for no_color in [false, true] {
                        let mut app = App::new(lang, view);
                        app.handle(key(KeyCode::F(1)));
                        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
                        terminal.draw(|f| render(f, &app, no_color)).unwrap();
                        let buffer = terminal.backend().buffer();
                        let visible = buffer
                            .content()
                            .iter()
                            .map(|c| c.symbol())
                            .collect::<String>();
                        for text in [
                            lang.choose("PARA QUE SERVE", "PURPOSE"),
                            lang.choose("O QUE SIGNIFICA", "WHAT IT MEANS"),
                            lang.choose("O QUE POSSO FAZER", "AVAILABLE ACTIONS"),
                            "DEMO: ",
                            "F1/Esc",
                        ] {
                            assert!(visible.contains(text), "{w}x{h} {view:?}: missing {text}");
                        }
                        let page = match view {
                            View::Graph => lang.choose("[1 Grafo]", "[1 Graph]"),
                            View::Query => lang.choose("[2 Consulta]", "[2 Query]"),
                            View::Memory => lang.choose("[3 Memória]", "[3 Memory]"),
                            View::Disk => lang.choose("[4 Disco]", "[4 Disk]"),
                        };
                        assert!(visible.contains(page));
                        if no_color {
                            assert!(buffer
                                .content()
                                .iter()
                                .all(|c| c.fg == Color::Reset && c.bg == Color::Reset));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn navigation_arrows_keep_focus_until_page_is_opened() {
        let mut app = App::new(Language::En, View::Graph);
        for view in [View::Query, View::Memory, View::Disk, View::Graph] {
            app.handle(key(KeyCode::Down));
            assert_eq!(app.view, view);
            assert_eq!(app.focus, Focus::Navigation);
        }
        app.handle(key(KeyCode::Tab));
        assert_eq!(app.focus, Focus::Main);
    }

    #[test]
    fn unicode_and_bounded_paste() {
        let mut app = App::new(Language::PtBr, View::Query);
        app.handle(key(KeyCode::Enter));
        app.query.clear();
        app.paste("ação🟢");
        app.handle(key(KeyCode::Backspace));
        assert_eq!(app.query, "ação");
        app.paste(&"界".repeat(1000));
        assert_eq!(app.query.chars().count(), 512);
    }
    #[test]
    fn guided_queries_use_fixture_and_reject_edits() {
        let mut app = App::new(Language::En, View::Query);
        app.run_query();
        assert!(app.result.contains("Ana Silva"));
        app.load_example(1);
        app.run_query();
        assert!(app.result.contains("Ana Silva  →  Aurora Labs"));
        app.query.push_str(" DELETE c");
        app.handle(key(KeyCode::Enter));
        assert!(app.editing);
        app.handle(key(KeyCode::F(5)));
        assert!(app.editing);
        app.run_query();
        assert!(app.result.contains("Unsupported demo query"));
        let mut pt = App::new(Language::PtBr, View::Query);
        pt.query.push('!');
        pt.run_query();
        assert!(pt.result.contains("fora dos exemplos"));
    }
    #[test]
    fn selection_updates_inspector() {
        let mut app = App::new(Language::En, View::Graph);
        assert!(app.selected_description().contains("Ana Silva"));
        app.focus = Focus::Main;
        app.handle(key(KeyCode::Down));
        assert!(app.selected_description().contains("Bruno Costa"));
        app.set_view(View::Disk);
        app.handle(key(KeyCode::Down));
        assert!(app.selected_description().contains("version metadata"));
        app.set_view(View::Memory);
        app.handle(key(KeyCode::Down));
        assert!(app.selected_description().contains("inside this 10 GiB"));
    }
    #[test]
    fn all_views_render_at_required_sizes() {
        for (w, h) in [(160, 48), (120, 36), (80, 24)] {
            for view in [View::Graph, View::Query, View::Memory, View::Disk] {
                let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
                let app = App::new(Language::En, view);
                terminal.draw(|f| render(f, &app, false)).unwrap();
                let text = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|c| c.symbol())
                    .collect::<String>();
                assert!(text.contains("BRAINWIRE"));
                assert!(text.contains("DEMO"));
                assert!(text.contains(view.name()));
                if view == View::Graph {
                    assert!(text.contains("◆00"));
                }
            }
        }
    }
    #[test]
    fn query_results_and_unsupported_status_remain_reachable_at_80x24() {
        let mut app = App::new(Language::En, View::Query);
        app.focus = Focus::Main;
        app.run_query();
        for _ in 0..6 {
            app.handle(key(KeyCode::Down));
        }
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| render(f, &app, false)).unwrap();
        let visible = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(visible.contains("Mateus Freitas"));

        app.handle(key(KeyCode::Enter));
        app.paste(&"x".repeat(512));
        app.handle(key(KeyCode::F(5)));
        terminal.draw(|f| render(f, &app, false)).unwrap();
        let visible = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(visible.contains("Unsupported demo query"));
        assert!(app.query.chars().count() <= 512);
    }
    #[test]
    fn graph_highlight_follows_selection() {
        let mut app = App::new(Language::En, View::Graph);
        let mut terminal = Terminal::new(TestBackend::new(120, 36)).unwrap();
        terminal.draw(|f| render(f, &app, false)).unwrap();
        let before = terminal.backend().buffer().clone();
        app.focus = Focus::Main;
        app.handle(key(KeyCode::Down));
        terminal.draw(|f| render(f, &app, false)).unwrap();
        assert_ne!(&before, terminal.backend().buffer());
        assert!(app.selected_description().contains("Bruno"));
    }
    #[test]
    fn no_color_and_languages_render() {
        for lang in [Language::En, Language::PtBr] {
            for no_color in [true, false] {
                let mut terminal = Terminal::new(TestBackend::new(120, 36)).unwrap();
                let app = App::new(lang, View::Query);
                terminal.draw(|f| render(f, &app, no_color)).unwrap();
                let text = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|c| c.symbol())
                    .collect::<String>();
                assert!(text.contains(if lang == Language::En {
                    "F5 runs"
                } else {
                    "F5 executa"
                }));
                if no_color {
                    assert!(terminal
                        .backend()
                        .buffer()
                        .content()
                        .iter()
                        .all(|c| c.fg == Color::Reset && c.bg == Color::Reset));
                }
            }
        }
    }
}

#[cfg(test)]
mod composition_tests {
    use super::*;
    fn text(app: &App, w: u16, h: u16, no_color: bool) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| render(f, app, no_color)).unwrap();
        if no_color {
            assert!(terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .all(|c| c.fg == Color::Reset && c.bg == Color::Reset));
        }
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }
    #[test]
    fn disk_and_memory_keep_core_composition_at_every_size() {
        for (w, h) in [(160, 48), (120, 36), (80, 24)] {
            for no_color in [false, true] {
                let mut app = App::new(Language::En, View::Disk);
                let disk = text(&app, w, h, no_color);
                for expected in [
                    "SYNTHETIC",
                    "CURRENT",
                    "MANIFEST-000001",
                    "000004.log",
                    "000011.sst",
                    "OPTIONS-000003",
                    "LOCK",
                    "TYPE",
                    "SIZE",
                    "MEMTABLE",
                    "L0",
                    "L1",
                    "L2",
                    "F1 help",
                ] {
                    assert!(disk.contains(expected), "{w}x{h}: {expected}");
                }
                if w >= 100 {
                    for expected in [
                        "Graph preview",
                        "WHAT IS IT?",
                        "WHY DOES IT EXIST?",
                        "memory.cache",
                    ] {
                        assert!(disk.contains(expected));
                    }
                }
                if w == 160 {
                    assert!(disk.contains("checkpoints"));
                }
                app.view = View::Memory;
                let memory = text(&app, w, h, no_color);
                for expected in [
                    "HEADROOM",
                    "16 GiB",
                    "11.5 GiB",
                    "4.5 GiB",
                    "7.2 / 10 GiB",
                    "0.6 / 1 GiB",
                    "inside cache",
                ] {
                    assert!(memory.contains(expected), "{w}x{h}: {expected}");
                }
                if w >= 120 {
                    assert!(memory.contains("included"));
                }
            }
        }
    }
    #[test]
    fn inspector_scroll_is_independent_and_cannot_erase_explanation() {
        let mut app = App::new(Language::En, View::Disk);
        app.selected_file = 3;
        app.focus = Focus::Inspector;
        app.handle(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.selected_file, 3);
        assert_eq!(app.inspector_scroll, 1);
        for _ in 0..40 {
            app.handle(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        }
        let compact = text(&app, 80, 24, false);
        assert!(compact.contains("persisted key/value"));
        app.set_view(View::Graph);
        assert_eq!(app.inspector_scroll, 0);
        assert!(text(&app, 80, 24, false).contains("name: Ana Silva"));
    }
    #[test]
    fn wrapped_examples_and_overflow_hint_are_visible_in_compact_query() {
        let mut app = App::new(Language::En, View::Query);
        app.load_example(1);
        let query = text(&app, 80, 24, false);
        assert!(query.contains("co.name LIMIT 12"));
        app.query = "界".repeat(150);
        assert!(text(&app, 80, 24, false).contains("MORE TEXT: Enter, PgUp/PgDn"));
        app.editing = true;
        let editing = text(&app, 80, 24, false);
        assert!(editing.contains("EDITING • PgUp/PgDn scroll"));
        assert!(!editing.contains("MORE TEXT: Enter"));
    }
    #[test]
    fn all_files_have_contextual_explanations_and_selection_stays_reachable() {
        let mut app = App::new(Language::En, View::Disk);
        app.focus = Focus::Main;
        for _ in 0..7 {
            let i = app.selected_file;
            let visible = text(&app, 120, 36, false);
            assert!(visible.contains(FILES[i].0));
            assert!(!app.selected_description().is_empty());
            if i == 3 {
                assert!(visible.contains("Column family: node"));
            }
            app.handle(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        assert_eq!(app.selected_file, 3);
    }
}
