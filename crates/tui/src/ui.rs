//! Adaptive observatory panels. All data here comes from the synthetic fixture.
use super::*;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{
        canvas::{Canvas, Line as CanvasLine},
        Block, Borders, Clear, Gauge, Paragraph, Wrap,
    },
    Frame,
};
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
        .border_style(style(BORDER, no_color))
        .title_style(style(color, no_color).add_modifier(Modifier::BOLD))
        .style(style(PALE, no_color))
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
            " ●─●─● ",
            style(ACCENT, no_color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "BRAINWIRE",
            style(ACCENT, no_color).add_modifier(Modifier::BOLD),
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
        Paragraph::new(header).block(block("", ACCENT, no_color)),
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
    let mut nav: Vec<Line> = names
        .iter()
        .enumerate()
        .map(|(i, n)| {
            Line::from(Span::styled(
                format!(" {} {}", if i == app.view.index() { "▶" } else { " " }, n),
                if i == app.view.index() {
                    selected_style(no_color)
                } else {
                    style(PALE, no_color)
                },
            ))
        })
        .collect();
    nav.extend([
        Line::raw(""),
        Line::from(Span::styled(
            app.lang.choose(" 1-4 telas", " 1-4 views"),
            style(CYAN, no_color),
        )),
        Line::from(Span::styled(
            app.lang.choose(" Tab painéis", " Tab panels"),
            style(CYAN, no_color),
        )),
    ]);
    let detail_height = if area.height < 30 { 5 } else { 8 };
    let full_explain = app.view == View::Disk && area.width >= 100;
    let mut sidebar_area = body[0];
    if full_explain {
        sidebar_area.height = sidebar_area.height.saturating_sub(detail_height);
    }
    let sidebar = Layout::vertical([Constraint::Length(9), Constraint::Min(1)]).split(sidebar_area);
    let nav_area = if area.width >= 100 {
        sidebar[0]
    } else {
        body[0]
    };
    if area.width >= 100 {
        graph(frame, app, sidebar[1], no_color, true);
    }
    frame.render_widget(
        Paragraph::new(nav).block(block(app.lang.choose("Vistas", "Views"), CYAN, no_color)),
        nav_area,
    );
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(detail_height)])
        .split(body[1]);
    let main = right[0];
    let mut inspector = right[1];
    if full_explain {
        inspector.x = sections[1].x;
        inspector.width = sections[1].width;
    }
    match app.view {
        View::Graph => {
            graph(frame, app, main, no_color, false);
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
                    app.lang.choose(
                        "Enter edita/rola • ←/→ exemplos",
                        "Enter edit/scroll • ←/→ examples",
                    )
                }
            );
            let title = if Line::from(app.query.as_str()).width()
                > query_parts[0].width.saturating_sub(2) as usize * 3
            {
                format!(
                    "{} • {}",
                    if app.editing {
                        app.lang
                            .choose("EDITANDO • PgUp/PgDn rola", "EDITING • PgUp/PgDn scroll")
                    } else {
                        app.lang.choose(
                            "TEXTO LONGO: Enter, PgUp/PgDn",
                            "MORE TEXT: Enter, PgUp/PgDn",
                        )
                    },
                    app.query_scroll
                )
            } else {
                title
            };
            frame.render_widget(
                Paragraph::new(app.query.chars().skip(app.query_scroll).collect::<String>())
                    .wrap(Wrap { trim: false })
                    .block(block(&title, ACCENT, no_color)),
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
        View::Memory => memory(frame, app, main, no_color),
        View::Disk => disk(frame, app, main, no_color),
    }
    let explanation = if app.view == View::Graph {
        format!(
            "{}\nid: {:02}   label: {}   name: {}\n{}",
            app.selected_description(),
            app.selected_node,
            app.fixture.nodes[app.selected_node].label,
            app.fixture.nodes[app.selected_node].name,
            app.lang.choose(
                "C=Cliente  O=Empresa  P=Projeto  *=aresta selecionada",
                "C=Customer  O=Company  P=Project  *=selected edge"
            )
        )
    } else if app.view == View::Disk && area.width >= 100 {
        app.lang.choose("memory.cache  •  CONFIGURADO 10 GiB / USO SINTÉTICO 7.2 GiB\nPool compartilhado de leituras e memtables. Memtables: 0.6 / 1 GiB, incluídas no cache.\nMais cache reduz leituras; deixa menos RAM para consultas. Orçamento, não RAM reservada.", "memory.cache  •  CONFIGURED 10 GiB / SYNTHETIC USED 7.2 GiB\nShared pool for cached reads and memtables. Memtables: 0.6 / 1 GiB, included in cache.\nMore cache reduces disk reads; leaves less RAM for queries. A budget, not reserved RAM.").into()
    } else {
        app.selected_description()
    };
    frame.render_widget(
        scrolled_text(
            explanation,
            inspector,
            if app.help || full_explain {
                0
            } else {
                app.inspector_scroll
            },
        )
        .block(block(
            if app.help {
                app.lang.choose("Ajuda", "Help")
            } else {
                app.lang.choose(
                    "Inspetor / explicação • ↑↓ rola",
                    "Inspector / explanation • ↑↓ scroll",
                )
            },
            if app.focus == Focus::Inspector {
                ACCENT
            } else {
                CYAN
            },
            no_color,
        )),
        inspector,
    );
    let page_names = if app.lang == Language::PtBr {
        ["Grafo", "Consulta", "Memória", "Disco"]
    } else {
        ["Graph", "Query", "Memory", "Disk"]
    };
    let pages = Line::from(
        page_names
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let active = index == app.view.index();
                Span::styled(
                    if active {
                        format!(" [{} {}] ", index + 1, name)
                    } else {
                        format!("  {} {}  ", index + 1, name)
                    },
                    if active {
                        selected_style(no_color)
                    } else {
                        style(CYAN, no_color)
                    },
                )
            })
            .collect::<Vec<_>>(),
    );
    let actions = if app.help {
        Line::from(app.lang.choose(
            " F1/?/Esc fecha ajuda • q sai",
            " F1/?/Esc close help • q quits",
        ))
    } else if app.editing {
        shortcut_line(
            &[
                ("F5", app.lang.choose("roda", "run")),
                (
                    "Enter/Esc",
                    app.lang.choose("termina edição", "finish editing"),
                ),
                ("PgUp/Dn", app.lang.choose("rola", "scroll")),
                ("F1", app.lang.choose("ajuda", "help")),
            ],
            no_color,
        )
    } else if app.focus == Focus::Inspector {
        shortcut_line(
            &[
                ("↑↓", app.lang.choose("rola", "scroll")),
                ("Esc", app.lang.choose("volta", "back")),
                ("Tab", app.lang.choose("painéis", "panels")),
                ("F1/?", app.lang.choose("ajuda", "help")),
                ("q", app.lang.choose("sai", "quit")),
            ],
            no_color,
        )
    } else if app.view == View::Query {
        shortcut_line(
            &[
                ("←→", app.lang.choose("exemplos", "examples")),
                ("Enter", app.lang.choose("edita", "edit")),
                ("F5", app.lang.choose("roda", "run")),
                ("Tab", app.lang.choose("painéis", "panels")),
                ("F1/?", app.lang.choose("ajuda", "help")),
                ("q", app.lang.choose("sai", "quit")),
            ],
            no_color,
        )
    } else {
        shortcut_line(
            &[
                ("↑↓", app.lang.choose("escolhe", "select")),
                ("Enter", app.lang.choose("detalhes", "details")),
                ("Tab", app.lang.choose("painéis", "panels")),
                ("F1/?", app.lang.choose("ajuda", "help")),
                ("q", app.lang.choose("sai", "quit")),
            ],
            no_color,
        )
    };
    frame.render_widget(
        Paragraph::new(vec![pages, actions]).style(style(PALE, no_color)),
        sections[2],
    );
    if app.help {
        help_overlay(frame, app, sections[1], no_color);
    }
}

fn shortcut_line(items: &[(&str, &str)], no_color: bool) -> Line<'static> {
    Line::from(
        items
            .iter()
            .flat_map(|(key, label)| {
                [
                    Span::styled(
                        format!(" {key} "),
                        style(ACCENT, no_color).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("{label} "), style(PALE, no_color)),
                ]
            })
            .collect::<Vec<_>>(),
    )
}

fn help_overlay(frame: &mut Frame, app: &App, body: Rect, no_color: bool) {
    let area = Rect::new(
        body.x + 1,
        body.y,
        body.width.saturating_sub(2),
        body.height,
    );
    let (purpose, meaning, actions) = match app.view {
        View::Graph => (
            app.lang.choose("Explore os nós e suas conexões no exemplo.", "Explore nodes and their connections in the example."),
            app.lang.choose("Nós são entidades. Linhas são relações; * marca as selecionadas.\nC=Customer, O=Company, P=Project; ◆ marca o nó selecionado.", "Nodes are entities. Lines are relationships; * marks selected links.\nC=Customer, O=Company, P=Project; ◆ marks the selected node."),
            app.lang.choose("1-4: muda de tela. Tab: muda de painel. ↑↓: escolhe nó.\nEnter: abre detalhes. Esc: volta ao conteúdo.", "1-4: change view. Tab: change panel. ↑↓: select node.\nEnter: open details. Esc: return to content."),
        ),
        View::Query => (
            app.lang.choose("Experimente dois exemplos guiados de Cypher.", "Try two guided Cypher examples."),
            app.lang.choose("MATCH encontra entidades/relações; RETURN escolhe as colunas.\nLIMIT limita linhas. Resultados vêm dos dados sintéticos da demo.\nSomente os dois exemplos exatos são suportados nesta demo.", "MATCH finds entities/relationships; RETURN chooses columns.\nLIMIT caps rows. Results come from the synthetic demo fixture.\nOnly the two exact examples are supported in this demo."),
            app.lang.choose("←→: exemplos. Enter: edita. F5: executa. ↑↓: rola resultados.\nNo editor: digite/cole; PgUp/PgDn rola; Enter/Esc termina edição.", "←→: examples. Enter: edit. F5: run. ↑↓: scroll results.\nIn editor: type/paste; PgUp/PgDn scroll; Enter/Esc finish editing."),
        ),
        View::Memory => (
            app.lang.choose("Entenda orçamentos e uso de memória.", "Understand memory budgets and usage."),
            app.lang.choose("Configurado é orçamento, não RAM reservada; usado é ilustrativo.\nCache guarda leituras e inclui memtables (buffers de escrita).\nRSS é memória física do processo; não some RSS e cache.", "Configured is a budget, not reserved RAM; used is illustrative.\nCache stores reads and includes memtables (write buffers).\nRSS is process resident memory; do not add RSS and cache."),
            app.lang.choose("↑↓: escolhe indicador. Tab: alterna para explicação.\nNo inspetor, ↑↓ rola o texto.", "↑↓: select metric. Tab: toggle explanation.\nIn the inspector, ↑↓ scrolls text."),
        ),
        View::Disk => (
            app.lang.choose("Veja para que serve cada arquivo do armazenamento.", "Learn what each storage file does."),
            app.lang.choose("WAL registra escritas; SST guarda dados ordenados em disco.\nMANIFEST registra as tabelas; CURRENT aponta ao manifesto ativo.\nMemtable → L0 → L1 → L2 mostra o fluxo ilustrativo da escrita.", "WAL records writes; SST stores sorted data on disk.\nMANIFEST tracks tables; CURRENT points to the active manifest.\nMemtable → L0 → L1 → L2 illustrates the storage write flow."),
            app.lang.choose("↑↓: escolhe arquivo. Tab: alterna para explicação.\nNo inspetor, ↑↓ rola o texto.", "↑↓: select file. Tab: toggle explanation.\nIn the inspector, ↑↓ scrolls text."),
        ),
    };
    let mut lines = Vec::new();
    for (label, text) in [
        (app.lang.choose("PARA QUE SERVE", "PURPOSE"), purpose),
        (app.lang.choose("O QUE SIGNIFICA", "WHAT IT MEANS"), meaning),
        (
            app.lang.choose("O QUE POSSO FAZER", "AVAILABLE ACTIONS"),
            actions,
        ),
    ] {
        lines.push(Line::from(Span::styled(
            label,
            style(ACCENT, no_color).add_modifier(Modifier::BOLD),
        )));
        lines.extend(text.lines().map(Line::from));
        lines.push(Line::from(""));
    }
    lines.push(Line::from(app.lang.choose(
        "DEMO: dados sintéticos; não altera arquivos nem configuração.",
        "DEMO: synthetic data; does not change files or configuration.",
    )));
    let title = format!(
        " {} / {} • F1/?/Esc {} ",
        app.lang.choose("Guia rápido", "Quick guide"),
        app.view.display(app.lang),
        app.lang.choose("fecha", "close")
    );
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(block(&title, ACCENT, no_color)),
        area,
    );
}

fn selected_style(no_color: bool) -> Style {
    selection_style(no_color)
}

fn position(id: usize) -> (f64, f64) {
    let cluster = id / 12;
    let angle = (id % 12) as f64 * std::f64::consts::TAU / 12.0 + cluster as f64 * 0.25;
    let (cx, cy) = [(22.0, 60.0), (72.0, 67.0), (48.0, 25.0)][cluster];
    (cx + angle.cos() * 16.0, cy + angle.sin() * 19.0)
}
fn graph(frame: &mut Frame, app: &App, area: Rect, no_color: bool, mini: bool) {
    let title = if mini {
        app.lang.choose("Prévia do grafo", "Graph preview")
    } else {
        app.lang
            .choose("Grafo • 36 nós / 64 arestas", "Graph • 36 nodes / 64 edges")
    };
    let color = |c| if no_color { Color::Reset } else { c };
    let mut canvas_area = area;
    let canvas_block = if mini {
        let panel = block(title, CYAN, no_color);
        canvas_area = panel.inner(area);
        frame.render_widget(panel, area);
        canvas_area.height = canvas_area.height.min(canvas_area.width / 2 + 1);
        let legend = Rect::new(
            canvas_area.x,
            canvas_area.bottom(),
            canvas_area.width,
            area.bottom().saturating_sub(canvas_area.bottom() + 1),
        );
        frame.render_widget(
            Paragraph::new(app.lang.choose(
                "\n36 nós / 64 arestas\nC Cliente\nO Empresa\nP Projeto\n\nDEMO / SINTÉTICO",
                "\n36 nodes / 64 edges\nC Customer\nO Company\nP Project\n\nDEMO / SYNTHETIC",
            ))
            .style(style(CYAN, no_color))
            .wrap(Wrap { trim: false }),
            legend,
        );
        Block::default()
    } else {
        block(title, CYAN, no_color)
    };
    let canvas = Canvas::default()
        .block(canvas_block)
        .background_color(color(BG))
        .x_bounds([0.0, 100.0])
        .y_bounds([0.0, 100.0])
        .paint(|ctx| {
            for selected in [false, true] {
                for edge in &app.fixture.edges {
                    let incident = edge.from == app.selected_node || edge.to == app.selected_node;
                    if incident != selected {
                        continue;
                    }
                    let (x1, y1) = position(edge.from);
                    let (x2, y2) = position(edge.to);
                    ctx.draw(&CanvasLine {
                        x1,
                        y1,
                        x2,
                        y2,
                        color: color(if incident {
                            ACCENT
                        } else {
                            Color::Rgb(50, 67, 96)
                        }),
                    });
                    if incident && !mini {
                        ctx.print(
                            (x1 + x2) / 2.0,
                            (y1 + y2) / 2.0,
                            Span::styled("*", style(ACCENT, no_color)),
                        );
                    }
                }
            }
            ctx.layer();
            for node in &app.fixture.nodes {
                let (x, y) = position(node.id);
                let selected = node.id == app.selected_node;
                let tag = if mini {
                    if selected { "◆" } else { "•" }.to_string()
                } else {
                    format!(
                        "{}{:02}",
                        if selected {
                            '◆'
                        } else {
                            match node.label {
                                "Customer" => 'C',
                                "Company" => 'O',
                                _ => 'P',
                            }
                        },
                        node.id
                    )
                };
                ctx.print(
                    x,
                    y,
                    Span::styled(
                        tag,
                        if selected {
                            selected_style(no_color)
                        } else {
                            style(if node.id < 12 { ACCENT } else { CYAN }, no_color)
                        },
                    ),
                );
            }
        });
    frame.render_widget(canvas, canvas_area);
}
const FILE_TYPES: [&str; 7] = [
    "Pointer",
    "Metadata",
    "WAL",
    "SST",
    "Config",
    "Diagnostic",
    "Lock",
];
fn disk(frame: &mut Frame, app: &App, area: Rect, no_color: bool) {
    let wide = frame.area().width >= 100;
    let cols =
        Layout::horizontal([Constraint::Percentage(65), Constraint::Percentage(35)]).split(area);
    let center = if wide { cols[0] } else { area };
    let rows = Layout::vertical([
        Constraint::Min(10),
        Constraint::Length(if area.height > 22 { 11 } else { 5 }),
    ])
    .split(center);
    let browser = block(
        app.lang.choose(
            "Disco e arquivos /data/sandbox/",
            "Disk & Files /data/sandbox/",
        ),
        ACCENT,
        no_color,
    );
    let inner = browser.inner(rows[0]);
    frame.render_widget(browser, rows[0]);
    let show_tree = center.width >= 76;
    let files = if show_tree {
        let split = Layout::horizontal([Constraint::Length(21), Constraint::Min(1)]).split(inner);
        let tree = vec![
            Line::from("▼ /data"),
            Line::styled(" └─ sandbox", style(ACCENT, no_color)),
            Line::from("    ├─ graphs"),
            Line::from("    ├─ checkpoints"),
            Line::from("    └─ exports"),
            Line::from(" ├─ research"),
            Line::from(" └─ archive"),
        ];
        frame.render_widget(Paragraph::new(tree).style(style(CYAN, no_color)), split[0]);
        split[1]
    } else {
        inner
    };
    let name_width = if files.width >= 42 { 20 } else { 16 };
    let mut lines = vec![Line::styled(
        format!(
            "  {:<name_width$} {:<10} {}",
            app.lang.choose("ARQUIVO", "FILE"),
            app.lang.choose("TIPO", "TYPE"),
            app.lang.choose("TAMANHO", "SIZE")
        ),
        style(CYAN, no_color),
    )];
    let spaced = files.height >= 17;
    if spaced {
        lines.push(Line::styled(
            "─".repeat(files.width as usize),
            style(CYAN, no_color),
        ));
    }
    for (i, (name, size, _)) in FILES.iter().enumerate() {
        lines.push(Line::styled(
            format!(
                "{} {:<name_width$} {:<10} {}",
                if i == app.selected_file { "▶" } else { " " },
                name,
                file_type(i, app.lang),
                size
            ),
            if i == app.selected_file {
                selected_style(no_color)
            } else {
                style(PALE, no_color)
            },
        ));
        if spaced {
            lines.push(Line::styled(
                "─".repeat(files.width as usize),
                style(BORDER, no_color),
            ));
        }
    }
    frame.render_widget(Paragraph::new(lines), files);
    storage_flow(frame, rows[1], no_color, app.lang);
    if wide {
        let i = app.selected_file;
        let description = format!(
            "{}{}",
            app.selected_description(),
            if i == 3 {
                app.lang.choose(
                    "\n\nFamília: node\nNível: L1 / Compressão: LZ4",
                    "\n\nColumn family: node\nLevel: L1 / Compression: LZ4",
                )
            } else {
                ""
            }
        );
        let detail = format!(
            "{}\n{} • {}\n\n{}\n{}\n\n{}\n{}\n\n{}\n{}\n\n{}",
            FILES[i].0,
            file_type(i, app.lang),
            FILES[i].1,
            app.lang.choose("O QUE É?", "WHAT IS IT?"),
            description,
            app.lang.choose("POR QUE EXISTE?", "WHY DOES IT EXIST?"),
            file_purpose(i, app.lang),
            app.lang.choose("RELACIONADOS", "RELATED"),
            app.lang.choose(
                "Memtables / WAL / Compactação",
                "Memtables / WAL / Compaction"
            ),
            app.lang.choose(
                "SINTÉTICO • arquivos ilustrativos. F1 ou ? explica; Tab, ↑↓ rola.",
                "SYNTHETIC • illustrative files. F1 or ? explains; Tab, ↑↓ scroll."
            )
        );
        frame.render_widget(
            scrolled_text(detail, cols[1], app.inspector_scroll).block(block(
                app.lang.choose("Arquivo selecionado", "Selected file"),
                if app.focus == Focus::Inspector {
                    ACCENT
                } else {
                    CYAN
                },
                no_color,
            )),
            cols[1],
        );
    }
}
fn memory(frame: &mut Frame, app: &App, area: Rect, no_color: bool) {
    let panel = block(
        app.lang
            .choose("Memória • cenário sintético", "Memory • synthetic scenario"),
        ACCENT,
        no_color,
    );
    let inner = panel.inner(area);
    frame.render_widget(panel, area);
    let rows = Layout::vertical([Constraint::Length(5), Constraint::Min(5)]).split(inner);
    let cards = Layout::horizontal([
        Constraint::Percentage(34),
        Constraint::Percentage(33),
        Constraint::Percentage(33),
    ])
    .split(rows[0]);
    for (i, (title, value)) in [
        (app.lang.choose("LIMITE", "LIMIT"), "16 GiB"),
        (
            app.lang.choose("USO SIMULADO", "SIMULATED USED"),
            "11.5 GiB",
        ),
        (app.lang.choose("LIVRE", "HEADROOM"), "4.5 GiB"),
    ]
    .iter()
    .enumerate()
    {
        frame.render_widget(
            Paragraph::new(format!("\n {value}"))
                .style(style(ACCENT, no_color).add_modifier(Modifier::BOLD))
                .block(block(title, CYAN, no_color)),
            cards[i],
        );
    }
    let compact = inner.height < 20;
    let mut lines = vec![Line::styled(
        app.lang.choose(
            "  ESCOPO        USO / ORÇAMENTO",
            "  SCOPE         USED / BUDGET",
        ),
        style(CYAN, no_color),
    )];
    for (i, (name, _, _)) in MEM.iter().enumerate() {
        let value = [
            "11.5 / 16 GiB",
            "7.2 / 10 GiB",
            app.lang
                .choose("0.6 / 1 GiB (no cache)", "0.6 / 1 GiB (inside cache)"),
            "0.8 / 2 GiB",
            "3.5 GiB",
        ][i];
        let label = if app.lang == Language::PtBr {
            MEM_PT_NAMES[i]
        } else {
            name
        };
        lines.push(Line::styled(
            format!(
                "{} {:<12} {}",
                if i == app.selected_memory { "▶" } else { " " },
                label,
                value
            ),
            if i == app.selected_memory {
                selected_style(no_color)
            } else {
                style(PALE, no_color)
            },
        ));
    }
    if !compact {
        lines.extend([
            Line::from(""),
            Line::styled(
                app.lang
                    .choose("CACHE 10 GiB configurado", "CACHE 10 GiB budget"),
                style(ACCENT, no_color),
            ),
            Line::from("┌───────────────────────────────────────────┐"),
            Line::from(format!(
                "│{:<43}│",
                app.lang.choose(
                    " Leituras + [ Memtables: 1 GiB ]",
                    " Cached reads + [ Memtables 1 GiB budget ]"
                )
            )),
            Line::from(format!(
                "│{:<43}│",
                app.lang.choose(
                    " Uso 7.2 GiB [ uso 0.6 GiB incluído ]",
                    " Used 7.2 GiB [ used 0.6 GiB included ]"
                )
            )),
            Line::from("└───────────────────────────────────────────┘"),
            Line::from(app.lang.choose(
                "Consulta: orçamento configurado, não imposto na demo.",
                "Query: configured budget, not enforced in this demo.",
            )),
            Line::from(app.lang.choose(
                "7.2 cache + 0.8 consulta + 3.5 outros = 11.5 GiB",
                "7.2 cache + 0.8 query + 3.5 other = 11.5 GiB",
            )),
        ]);
    }
    frame.render_widget(Paragraph::new(lines), rows[1]);
    if inner.height >= 27 {
        let gauge = Rect::new(inner.x, inner.bottom() - 3, inner.width, 3);
        frame.render_widget(
            Gauge::default()
                .ratio(11.5 / 16.0)
                .label(app.lang.choose(
                    "11.5 GiB usados / 4.5 GiB livres",
                    "11.5 GiB used / 4.5 GiB headroom",
                ))
                .gauge_style(style(ACCENT, no_color))
                .block(block(
                    app.lang
                        .choose("Contêiner • simulado", "Container • simulated"),
                    CYAN,
                    no_color,
                )),
            gauge,
        );
    }
}
fn file_purpose(i: usize, lang: Language) -> &'static str {
    let en = [
        "Finds the latest metadata version during recovery.",
        "Tracks which SST files form the current database version.",
        "Replays recent committed writes after a crash, before SST flush.",
        "Stores sorted graph records beyond RAM. Flush and compaction create these tables.",
        "Records the storage options used to open this database.",
        "Helps diagnose engine events and storage problems.",
        "Prevents two processes from opening the same database concurrently.",
    ];
    let pt = [
        "Localiza a versão atual dos metadados na recuperação.",
        "Registra quais arquivos SST compõem a versão atual do banco.",
        "Recupera gravações confirmadas após uma falha, antes do flush SST.",
        "Guarda registros ordenados além da RAM. Flush e compactação criam essas tabelas.",
        "Registra as opções usadas ao abrir o banco.",
        "Ajuda a diagnosticar eventos e problemas do motor.",
        "Impede dois processos de abrirem o mesmo banco simultaneamente.",
    ];
    if lang == Language::PtBr {
        pt[i]
    } else {
        en[i]
    }
}
fn storage_flow(frame: &mut Frame, area: Rect, no_color: bool, lang: Language) {
    let panel = block(
        lang.choose("LSM • fluxo ilustrativo", "LSM • illustrative storage flow"),
        CYAN,
        no_color,
    );
    let inner = panel.inner(area);
    frame.render_widget(panel, area);
    if inner.height < 5 {
        frame.render_widget(
            Paragraph::new(lang.choose(
                " MEMTABLE → L0 → L1 → L2\n RAM        DISCO / SST • flush → compactação",
                " MEMTABLE → L0 → L1 → L2\n RAM        ON DISK / SST • flush → compaction",
            ))
            .style(style(CYAN, no_color)),
            inner,
        );
        return;
    }
    let parts = Layout::vertical([Constraint::Min(3), Constraint::Length(2)]).split(inner);
    let stages = Layout::horizontal([Constraint::Percentage(25); 4]).split(parts[0]);
    for (i, name) in ["MEMTABLE →", "L0 →", "L1 →", "L2"].iter().enumerate() {
        let color = if i == 0 { ACCENT } else { CYAN };
        let stage = Rect::new(
            stages[i].x + 1,
            stages[i].y + 1,
            stages[i].width.saturating_sub(2),
            stages[i].height.saturating_sub(1),
        );
        let bars = "━".repeat(stage.width.saturating_sub(4) as usize);
        let text = (0..stage.height.saturating_sub(2))
            .map(|_| format!(" {bars}"))
            .collect::<Vec<_>>()
            .join("\n");
        frame.render_widget(
            Paragraph::new(text)
                .style(style(color, no_color))
                .block(block(name, color, no_color)),
            stage,
        );
    }
    frame.render_widget(
        Paragraph::new(lang.choose(
            " NA RAM       →       NO DISCO / SST\n flush        →       compactação",
            " IN RAM       →       ON DISK / SST\n flush        →       compaction",
        ))
        .style(style(CYAN, no_color)),
        parts[1],
    );
}

fn scrolled_text(text: String, area: Rect, scroll: u16) -> Paragraph<'static> {
    let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
    let max = paragraph
        .line_count(area.width.saturating_sub(2))
        .saturating_sub(area.height.saturating_sub(2) as usize);
    paragraph.scroll((scroll.min(max.min(u16::MAX as usize) as u16), 0))
}

fn file_type(i: usize, lang: Language) -> &'static str {
    if lang == Language::PtBr {
        [
            "Ponteiro",
            "Metadados",
            "WAL",
            "SST",
            "Config",
            "Diagnóstico",
            "Trava",
        ][i]
    } else {
        FILE_TYPES[i]
    }
}

#[cfg(test)]
mod navigation_tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn compact_demo_footer_keeps_contextual_actions_visible() {
        for lang in [Language::En, Language::PtBr] {
            for view in [View::Graph, View::Query, View::Memory, View::Disk] {
                for focus in [Focus::Main, Focus::Inspector] {
                    let mut app = App::new(lang, view);
                    app.focus = focus;
                    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
                    terminal.draw(|frame| render(frame, &app, true)).unwrap();
                    let buffer = terminal.backend().buffer();
                    let footer = (0..80)
                        .map(|x| buffer[(x, 23)].symbol())
                        .collect::<String>();
                    for action in ["F1", "Tab", lang.choose("q sai", "q quit")] {
                        assert!(footer.contains(action), "missing {action}: {footer}");
                    }
                    if focus == Focus::Inspector {
                        assert!(footer.contains(lang.choose("Esc volta", "Esc back")));
                    }
                    if view == View::Query && focus == Focus::Main {
                        assert!(footer.contains(lang.choose("F5 roda", "F5 run")));
                    }
                }
            }
        }
    }

    #[test]
    fn editing_keeps_question_mark_as_text_and_shows_f1_help() {
        for lang in [Language::En, Language::PtBr] {
            let mut app = App::new(lang, View::Query);
            app.handle(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            app.handle(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
            assert!(app.query.ends_with('?'));

            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
            terminal.draw(|frame| render(frame, &app, true)).unwrap();
            let screen = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(screen.contains("F1"));
            assert!(!screen.contains("F1/?"));
        }
    }
}
