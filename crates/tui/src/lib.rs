//! A bounded, synthetic terminal observatory for Brainwire.
use std::io::{self, IsTerminal, Stdout};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::{CrosstermBackend, TestBackend};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

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
const MEM_PT_VALUES: [&str; 5] = [
    "limite 16 GiB / uso simulado 11.5 GiB / livre 4.5 GiB",
    "configurado 10 GiB / uso simulado 7.2 GiB",
    "orçamento 1 GiB / uso simulado 0.6 GiB",
    "configurado 2 GiB / uso simulado 0.8 GiB",
    "uso simulado 3.5 GiB",
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
            selected_file: 0,
            selected_memory: 0,
            example: 0,
            query: EXAMPLES[0].into(),
            result: String::new(),
            query_scroll: 0,
            result_scroll: 0,
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
        if self.editing {
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
        self.focus = Focus::Main;
        self.editing = false;
    }
    fn move_selection(&mut self, delta: isize) {
        if self.focus == Focus::Navigation {
            self.set_view(View::from_index(
                (self.view.index() as isize + delta).rem_euclid(4) as usize,
            ));
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

fn style(color: Color, no_color: bool) -> Style {
    if no_color {
        Style::default()
    } else {
        Style::default().fg(color).bg(BG)
    }
}
fn block<'a>(title: &'a str, color: Color, no_color: bool) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(style(color, no_color))
        .style(style(PALE, no_color))
}

fn usage_line(
    label: &'static str,
    filled: usize,
    value: &'static str,
    no_color: bool,
) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<11}["), style(PALE, no_color)),
        Span::styled("█".repeat(filled), style(GREEN, no_color)),
        Span::styled("░".repeat(20 - filled), style(CYAN, no_color)),
        Span::styled(format!("] {value}"), style(PALE, no_color)),
    ])
}

fn graph_lines(app: &App, width: usize, height: usize) -> Vec<Line<'static>> {
    if width < 4 || height < 3 {
        return vec![Line::from("Graph area too small")];
    }
    let w = width;
    let h = height;
    let mut cells = vec![vec![' '; w]; h];
    let pos = |n: &Node| -> (isize, isize) {
        let cluster = n.id / 12;
        let local = n.id % 12;
        if w < 90 || h < 20 {
            let third = w / 3;
            (
                (cluster * third + (local % 3 + 1) * third / 4) as isize,
                ((local / 3 + 1) * h / 5) as isize,
            )
        } else {
            let angle = (local as f64 / 12.0) * std::f64::consts::TAU + (cluster as f64 * 0.21);
            let center_x = (cluster as f64 + 0.5) * w as f64 / 3.0;
            let radius_x = w as f64 / 8.0;
            let center_y = h as f64 / 2.0;
            let radius_y = h as f64 * 0.4;
            (
                (center_x + radius_x * angle.cos()).round() as isize,
                (center_y + radius_y * angle.sin()).round() as isize,
            )
        }
    };
    for edge in &app.fixture.edges {
        let (x0, y0) = pos(&app.fixture.nodes[edge.from]);
        let (x1, y1) = pos(&app.fixture.nodes[edge.to]);
        let steps = (x1 - x0).abs().max((y1 - y0).abs()).max(1);
        let mark = if edge.from == app.selected_node || edge.to == app.selected_node {
            '*'
        } else {
            '·'
        };
        for step in 1..steps {
            let x = x0 + (x1 - x0) * step / steps;
            let y = y0 + (y1 - y0) * step / steps;
            if x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h {
                cells[y as usize][x as usize] = mark;
            }
        }
    }
    for n in &app.fixture.nodes {
        let (x, y) = pos(n);
        if x >= 0 && y >= 0 && (x as usize) + 2 < w && (y as usize) < h {
            let tag = format!(
                "{}{:02}",
                if n.id == app.selected_node {
                    '◆'
                } else {
                    match n.label {
                        "Customer" => 'C',
                        "Company" => 'O',
                        _ => 'P',
                    }
                },
                n.id
            );
            for (i, c) in tag.chars().enumerate() {
                cells[y as usize][x as usize + i] = c;
            }
        }
    }
    cells
        .into_iter()
        .map(|row| Line::from(row.into_iter().collect::<String>()))
        .collect()
}

pub fn render(frame: &mut Frame, app: &App, no_color: bool) {
    let area = frame.area();
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(2),
        ])
        .split(area);
    let header = Line::from(vec![
        Span::styled(
            " BRAINWIRE ",
            style(GREEN, no_color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            app.lang
                .choose("/ observatório de grafos    ", "/ graph observatory    "),
            style(PALE, no_color),
        ),
        Span::styled(
            format!(
                "{}   •   {}",
                app.lang.choose("DEMO / SINTÉTICO", "DEMO / SYNTHETIC"),
                app.view.display(app.lang)
            ),
            style(CYAN, no_color),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(header).block(block("", GREEN, no_color)),
        sections[0],
    );
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(if area.width < 100 { 16 } else { 21 }),
            Constraint::Min(1),
        ])
        .split(sections[1]);
    let names = if app.lang == Language::PtBr {
        if area.width < 100 {
            ["1 Grafo", "2 Consulta", "3 Memória", "4 Disco"]
        } else {
            ["1 Grafo", "2 Consulta", "3 Memória", "4 Disco/arquivos"]
        }
    } else if area.width < 100 {
        ["1 Graph", "2 Query", "3 Memory", "4 Disk"]
    } else {
        ["1 Graph", "2 Query", "3 Memory", "4 Disk & Files"]
    };
    let nav: Vec<Line> = names
        .iter()
        .enumerate()
        .map(|(i, n)| {
            Line::from(Span::styled(
                format!(" {} {}", if i == app.view.index() { "▶" } else { " " }, n),
                if i == app.view.index() {
                    style(GREEN, no_color).add_modifier(Modifier::BOLD)
                } else {
                    style(PALE, no_color)
                },
            ))
        })
        .collect();
    frame.render_widget(
        Paragraph::new(nav).block(block(
            app.lang.choose("Vistas", "Views"),
            if app.focus == Focus::Navigation {
                GREEN
            } else {
                CYAN
            },
            no_color,
        )),
        body[0],
    );
    let detail_height = if area.height < 30 { 5 } else { 8 };
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(detail_height)])
        .split(body[1]);
    let main = right[0];
    let inspector = right[1];
    match app.view {
        View::Graph => {
            let inner_width = main.width.saturating_sub(2) as usize;
            let inner_height = main.height.saturating_sub(2) as usize;
            let colored: Vec<Line> = graph_lines(app, inner_width, inner_height)
                .into_iter()
                .map(|line| {
                    let text = line.to_string();
                    Line::from(
                        text.chars()
                            .map(|c| {
                                let color = match c {
                                    'C' | '◆' | '*' => GREEN,
                                    'O' => CYAN,
                                    'P' => PALE,
                                    _ => Color::DarkGray,
                                };
                                Span::styled(c.to_string(), style(color, no_color))
                            })
                            .collect::<Vec<_>>(),
                    )
                })
                .collect();
            frame.render_widget(
                Paragraph::new(colored).block(block(
                    app.lang.choose(
                        "Grafo • 36 nós / 64 arestas • setas selecionam",
                        "Graph • 36 nodes / 64 edges • arrows select",
                    ),
                    GREEN,
                    no_color,
                )),
                main,
            );
        }
        View::Query => {
            let query_parts = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(5), Constraint::Min(5)])
                .split(main);
            let title = format!(
                "{} {}/2 • {}",
                app.lang.choose("Exemplo", "Example"),
                app.example + 1,
                if app.editing {
                    app.lang
                        .choose("EDITANDO • PgUp/PgDn rola", "EDITING • PgUp/PgDn scroll")
                } else {
                    app.lang
                        .choose("Enter edita • ←/→ exemplos", "Enter edit • ←/→ examples")
                }
            );
            frame.render_widget(
                Paragraph::new(app.query.as_str())
                    .scroll((0, app.query_scroll.min(u16::MAX as usize) as u16))
                    .block(block(&title, GREEN, no_color)),
                query_parts[0],
            );
            let result = if app.result.is_empty() {
                app.lang.choose(
                    "F5 executa o exemplo selecionado.",
                    "F5 runs the selected example.",
                )
            } else {
                &app.result
            };
            frame.render_widget(
                Paragraph::new(result)
                    .scroll((app.result_scroll.min(u16::MAX as usize) as u16, 0))
                    .wrap(Wrap { trim: false })
                    .block(block(
                        app.lang.choose(
                            "Resultado guiado • ↑↓ rola • F5 executa",
                            "Guided result • ↑↓ scroll • F5 run",
                        ),
                        CYAN,
                        no_color,
                    )),
                query_parts[1],
            );
        }
        View::Memory => {
            let mut lines = Vec::new();
            for (i, (name, value, _)) in MEM.iter().enumerate() {
                lines.push(Line::from(Span::styled(
                    format!(
                        "{} {:<12} {}",
                        if i == app.selected_memory { "▶" } else { " " },
                        if app.lang == Language::PtBr {
                            MEM_PT_NAMES[i]
                        } else {
                            name
                        },
                        if app.lang == Language::PtBr {
                            MEM_PT_VALUES[i]
                        } else {
                            value
                        }
                    ),
                    if i == app.selected_memory {
                        style(GREEN, no_color)
                    } else {
                        style(PALE, no_color)
                    },
                )));
            }
            lines.push(Line::from(""));
            lines.push(usage_line(
                app.lang.choose("Contêiner", "Container"),
                14,
                "11.5 / 16 GiB",
                no_color,
            ));
            lines.push(usage_line("Cache", 14, "7.2 / 10 GiB", no_color));
            lines.push(usage_line(
                "Memtables",
                12,
                app.lang
                    .choose("0.6 / 1 GiB (no cache)", "0.6 / 1 GiB (inside cache)"),
                no_color,
            ));
            lines.push(usage_line(
                app.lang.choose("Consulta", "Query"),
                8,
                app.lang.choose(
                    "0.8 / 2 GiB (sem limite nesta demo)",
                    "0.8 / 2 GiB (not enforced here)",
                ),
                no_color,
            ));
            frame.render_widget(
                Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .block(block(
                        app.lang.choose(
                            "Memória • simulação, não RSS medido",
                            "Memory • simulated, not measured RSS",
                        ),
                        GREEN,
                        no_color,
                    )),
                main,
            );
        }
        View::Disk => {
            let lines: Vec<Line> = FILES
                .iter()
                .enumerate()
                .map(|(i, (name, size, _))| {
                    Line::from(Span::styled(
                        format!(
                            "{} {:<20} {:>8}",
                            if i == app.selected_file { "▶" } else { " " },
                            name,
                            size
                        ),
                        if i == app.selected_file {
                            style(GREEN, no_color)
                        } else {
                            style(PALE, no_color)
                        },
                    ))
                })
                .collect();
            frame.render_widget(
                Paragraph::new(lines).block(block(
                    app.lang.choose(
                        "Disco e arquivos • ilustrativo, sem acesso ao disco",
                        "Disk & Files • illustrative, no filesystem access",
                    ),
                    GREEN,
                    no_color,
                )),
                main,
            );
        }
    }
    let explanation = if app.help {
        app.lang.choose("F1/Esc fecha ajuda. Tab muda foco. 1-4 abre vistas. Setas selecionam. Enter edita Query. F5 executa exemplo. q sai fora do editor.","F1/Esc closes help. Tab changes focus. 1-4 opens views. Arrows select. Enter edits Query. F5 runs example. q quits outside editor.").to_string()
    } else if app.view == View::Graph {
        format!(
            "{}\n{}",
            app.selected_description(),
            app.lang.choose(
                "C=Cliente  O=Empresa  P=Projeto  *=aresta selecionada",
                "C=Customer  O=Company  P=Project  *=selected edge"
            )
        )
    } else {
        app.selected_description()
    };
    frame.render_widget(
        Paragraph::new(explanation)
            .wrap(Wrap { trim: false })
            .block(block(
                if app.help {
                    app.lang.choose("Ajuda", "Help")
                } else {
                    app.lang
                        .choose("Inspetor / explicação", "Inspector / explanation")
                },
                if app.focus == Focus::Inspector {
                    GREEN
                } else {
                    CYAN
                },
                no_color,
            )),
        inspector,
    );
    let footer = if app.lang == Language::PtBr {
        if area.width < 100 {
            " F1 ajuda Tab foco 1-4 vistas ↑↓ seleciona Enter edita F5 roda Esc/q sai"
        } else {
            " F1 ajuda  Tab foco  1-4 vistas  ↑↓ seleciona  Enter inspeciona/edita  F5 executa  Esc volta  q sai  •  DEMO"
        }
    } else if area.width < 100 {
        " F1 help Tab focus 1-4 views ↑↓ select Enter edit F5 run Esc/q back"
    } else {
        " F1 help  Tab focus  1-4 views  ↑↓ select  Enter inspect/edit  F5 run  Esc back  q quit  •  DEMO"
    };
    frame.render_widget(
        Paragraph::new(footer).style(style(PALE, no_color)),
        sections[2],
    );
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
        execute!(io::stdout(), EnterAlternateScreen)
            .map_err(|e| format!("alternate screen setup: {e}"))?;
        guard.alternate = true;
        execute!(io::stdout(), crossterm::event::EnableBracketedPaste)
            .map_err(|e| format!("bracketed paste setup: {e}"))?;
        guard.paste = true;
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
        let before = graph_lines(&app, 80, 18)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<String>();
        assert!(before.contains('◆'));
        assert!(before.contains('*'));
        app.focus = Focus::Main;
        app.handle(key(KeyCode::Down));
        let after = graph_lines(&app, 80, 18)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<String>();
        assert_ne!(before, after);
        assert!(after.contains('◆'));
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
