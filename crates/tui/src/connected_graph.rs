//! Bounded outgoing relationships for one configured entity; no graph writes.
use crate::overview::{panel, terminal_text};
use crate::*;
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::Line,
    widgets::{
        canvas::{Canvas, Circle, Line as CanvasLine},
        Paragraph, Wrap,
    },
    Frame,
};
use std::time::Instant;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GraphSnapshot {
    pub node_label: String,
    pub entity_id: String,
    pub edge_type: String,
    pub target_label: String,
    pub found: bool,
    /// Each item is one relationship row, not a distinct neighbor count.
    pub neighbors: Vec<String>,
    pub has_more: bool,
}
pub(crate) struct State {
    pub(crate) snapshot: GraphSnapshot,
    pub(crate) selected: usize,
    sampled: Instant,
    error: Option<String>,
}
impl State {
    pub(crate) fn new(snapshot: GraphSnapshot) -> Self {
        Self {
            snapshot,
            selected: 0,
            sampled: Instant::now(),
            error: None,
        }
    }
    pub(crate) fn refresh(&mut self, result: Result<GraphSnapshot, String>) {
        match result {
            Ok(snapshot) => {
                self.selected = self
                    .snapshot
                    .neighbors
                    .get(self.selected)
                    .and_then(|id| snapshot.neighbors.iter().position(|new| new == id))
                    .unwrap_or(0)
                    .min(19);
                self.snapshot = snapshot;
                self.sampled = Instant::now();
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }
    pub(crate) fn handle(&mut self, code: KeyCode) {
        let last = self.snapshot.neighbors.len().min(20).saturating_sub(1);
        match code {
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.selected = (self.selected + 1).min(last),
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(5),
            KeyCode::PageDown => self.selected = (self.selected + 5).min(last),
            _ => {}
        }
    }
}
fn display(text: &str) -> String {
    terminal_text(&text.chars().take(256).collect::<String>())
}
pub(crate) fn render_at(
    frame: &mut Frame,
    area: Rect,
    state: &mut State,
    address: &str,
    graph: &str,
    lang: Language,
    no_color: bool,
) {
    let areas = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(if state.error.is_some() { 5 } else { 3 }),
        Constraint::Length(4),
        Constraint::Min(0),
        Constraint::Length(3),
    ])
    .split(area);
    frame.render_widget(
        Paragraph::new(format!(
            " BRAINWIRE / {} / {} @ {}",
            lang.choose("GRAFO", "GRAPH"),
            display(graph),
            display(address)
        ))
        .block(panel("", GREEN, no_color)),
        areas[0],
    );
    let count = if state.snapshot.found {
        state.snapshot.neighbors.len().min(20)
    } else {
        0
    };
    let more = state.snapshot.has_more || state.snapshot.neighbors.len() > 20;
    let mut status = format!(
        " {} • {} {}s • {} {} / 20{}",
        lang.choose("SNAPSHOT DO SERVIDOR", "SERVER SNAPSHOT"),
        lang.choose("idade", "age"),
        state.sampled.elapsed().as_secs(),
        count,
        lang.choose("saída", "outgoing rows"),
        if more {
            lang.choose(" • MAIS DISPONÍVEIS", " • MORE AVAILABLE")
        } else {
            ""
        }
    );
    if let Some(error) = &state.error {
        status.push_str(&format!("\n {}", display(error)));
    }
    frame.render_widget(
        Paragraph::new(status)
            .wrap(Wrap { trim: false })
            .block(panel(
                if state.error.is_some() {
                    lang.choose(
                        "DESATUALIZADO • atualização falhou",
                        "STALE • refresh failed",
                    )
                } else {
                    ""
                },
                CYAN,
                no_color,
            )),
        areas[1],
    );
    let scope = format!(
        " {}: {}\n -[:{}]-> {} • {}",
        display(&state.snapshot.node_label),
        display(&state.snapshot.entity_id),
        display(&state.snapshot.edge_type),
        display(&state.snapshot.target_label),
        lang.choose("somente leitura", "read only")
    );
    frame.render_widget(
        Paragraph::new(scope)
            .wrap(Wrap { trim: false })
            .block(panel(
                lang.choose("ENTIDADE CONFIGURADA", "CONFIGURED ENTITY"),
                GREEN,
                no_color,
            )),
        areas[2],
    );
    let body = if areas[3].width >= 100 {
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).split(areas[3])
    } else {
        Layout::vertical([
            Constraint::Length(5.min(areas[3].height / 2)),
            Constraint::Min(0),
        ])
        .split(areas[3])
    };
    if state.snapshot.found && body[0].height < 8 {
        let preview = if let Some(id) = state
            .snapshot
            .neighbors
            .get(state.selected)
            .filter(|_| count > 0)
        {
            format!(
                " ● 0 ───> ○ {:02} {}\n {}",
                state.selected + 1,
                display(id),
                lang.choose(
                    "Prévia da relação selecionada; IDs na lista abaixo.",
                    "Selected relationship preview; IDs listed below."
                )
            )
        } else {
            lang.choose(
                " ● 0 • nenhuma relação correspondente.",
                " ● 0 • no matching relationship.",
            )
            .into()
        };
        frame.render_widget(
            Paragraph::new(preview)
                .wrap(Wrap { trim: false })
                .block(panel(
                    lang.choose(
                        "PRÉVIA / RELAÇÃO SELECIONADA",
                        "PREVIEW / SELECTED RELATIONSHIP",
                    ),
                    GREEN,
                    no_color,
                )),
            body[0],
        );
    } else if state.snapshot.found {
        let color = if no_color { Color::Reset } else { GREEN };
        let cyan = if no_color { Color::Reset } else { CYAN };
        frame.render_widget(
            Canvas::default()
                .x_bounds([-12.0, 12.0])
                .y_bounds([-8.0, 8.0])
                .block(panel(
                    lang.choose(
                        "PRÉVIA / RELAÇÕES AMOSTRADAS",
                        "PREVIEW / SAMPLED RELATIONSHIPS",
                    ),
                    GREEN,
                    no_color,
                ))
                .paint(|ctx| {
                    ctx.draw(&Circle {
                        x: 0.0,
                        y: 0.0,
                        radius: 0.65,
                        color,
                    });
                    ctx.print(0.8, 0.0, "0");
                    for i in 0..count {
                        let angle = i as f64 * std::f64::consts::TAU / count as f64;
                        let (x, y) = (9.0 * angle.cos(), 5.5 * angle.sin());
                        let selected = i == state.selected;
                        ctx.draw(&CanvasLine {
                            x1: 0.0,
                            y1: 0.0,
                            x2: x,
                            y2: y,
                            color: if selected { color } else { cyan },
                        });
                        ctx.draw(&Circle {
                            x,
                            y,
                            radius: if selected { 0.6 } else { 0.3 },
                            color: if selected { color } else { cyan },
                        });
                        ctx.print(x, y, format!("{}", i + 1));
                    }
                }),
            body[0],
        );
    } else {
        frame.render_widget(
            Paragraph::new(lang.choose(
                " Entidade não encontrada.\n Nenhum nó foi criado.",
                " Entity not found.\n No node was created.",
            ))
            .wrap(Wrap { trim: false })
            .block(panel(lang.choose("PRÉVIA", "PREVIEW"), GREEN, no_color)),
            body[0],
        );
    }
    let capacity = usize::from(body[1].height.saturating_sub(2)).max(1);
    let start = state.selected.saturating_sub(capacity.saturating_sub(1));
    let mut lines = if count == 0 {
        vec![Line::raw(if !state.snapshot.found {
            lang.choose(" Entidade não encontrada.", " Entity not found.")
        } else {
            lang.choose(
                " Sem relações deste tipo na amostra.",
                " No matching outgoing relationships.",
            )
        })]
    } else {
        state
            .snapshot
            .neighbors
            .iter()
            .take(count)
            .enumerate()
            .skip(start)
            .take(capacity)
            .map(|(i, id)| {
                Line::styled(
                    format!(
                        "{} {:02} {}",
                        if i == state.selected { ">" } else { " " },
                        i + 1,
                        display(id)
                    ),
                    if i == state.selected && !no_color {
                        Style::default().fg(GREEN)
                    } else {
                        Style::default()
                    },
                )
            })
            .collect()
    };
    if count == 0 && state.snapshot.found {
        lines.push(Line::raw(lang.choose(
            " Outros tipos/direções não foram consultados.",
            " Other types/directions were not queried.",
        )));
    }
    frame.render_widget(
        Paragraph::new(lines).block(panel(
            lang.choose("DESTINOS / IDs PODEM REPETIR", "TARGETS / IDs MAY REPEAT"),
            CYAN,
            no_color,
        )),
        body[1],
    );
    frame.render_widget(
        Paragraph::new(lang.choose(
            " ↑↓ seleciona • PgUp/Dn rola • r atualiza • q sai",
            " ↑↓ select • PgUp/Dn scroll • r refresh • q quit",
        ))
        .block(panel(
            lang.choose(
                "Até 20 relações; não é o grau total. Sem propriedades.",
                "Up to 20 relationships; not total degree. No properties.",
            ),
            GREEN,
            no_color,
        )),
        areas[4],
    );
}
/// Refresh replaces the complete bounded sample; failures retain the old sample.
pub fn run_connected_graph(
    options: &Options,
    address: &str,
    graph: &str,
    mut fetch: impl FnMut() -> Result<GraphSnapshot, String>,
) -> Result<(), String> {
    let lang = Language::parse(&options.lang)?;
    if options.view != "graph" {
        return Err("connected graph requires --view graph".into());
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
    let mut state = State::new(fetch()?);
    if options.snapshot {
        let mut terminal = Terminal::new(TestBackend::new(options.width, options.height))
            .map_err(|e| e.to_string())?;
        terminal
            .draw(|f| render_at(f, f.area(), &mut state, address, graph, lang, no_color))
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
            .draw(|f| render_at(f, f.area(), &mut state, address, graph, lang, no_color))
            .map_err(|e| e.to_string())?;
        if event::poll(Duration::from_secs(1)).map_err(|e| e.to_string())? {
            if let Event::Key(key) = event::read().map_err(|e| e.to_string())? {
                if key.kind != event::KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Char('r') => state.refresh(fetch()),
                    code => state.handle(code),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(count: usize) -> GraphSnapshot {
        GraphSnapshot {
            node_label: "Ticket".into(),
            entity_id: "ticket-1".into(),
            edge_type: "HAS_COMMENT".into(),
            target_label: "TicketComment".into(),
            found: true,
            neighbors: (0..count).map(|i| format!("comment-{i}")).collect(),
            has_more: count >= 20,
        }
    }
    fn text(state: &mut State, lang: Language, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| render_at(f, f.area(), state, "server\x1b[31m", "graph\n", lang, true))
            .unwrap();
        assert!(terminal
            .backend()
            .buffer()
            .content
            .iter()
            .all(|c| c.fg == Color::Reset && c.bg == Color::Reset));
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect()
    }
    #[test]
    fn bounded_bilingual_real_relationships_preserve_direction_and_selection() {
        for lang in [Language::En, Language::PtBr] {
            for (w, h) in [(80, 24), (120, 30)] {
                let mut snapshot = sample(20);
                snapshot.neighbors[1] = "comment-0".into();
                let mut state = State::new(snapshot.clone());
                let rendered = text(&mut state, lang, w, h);
                for expected in [
                    "ticket-1",
                    "TicketComment",
                    "HAS_COMMENT",
                    "20",
                    lang.choose("MAIS DISPONÍVEIS", "MORE AVAILABLE"),
                    lang.choose("IDs PODEM REPETIR", "IDs MAY REPEAT"),
                    lang.choose("não é o grau total", "not total degree"),
                    "comment-0",
                ] {
                    assert!(
                        rendered.contains(expected),
                        "{w}x{h} {expected}: {rendered}"
                    );
                }
                assert_eq!(
                    state
                        .snapshot
                        .neighbors
                        .iter()
                        .filter(|id| id.as_str() == "comment-0")
                        .count(),
                    2
                );
                for _ in 0..8 {
                    state.handle(KeyCode::PageDown);
                }
                assert_eq!(state.selected, 19);
                assert!(text(&mut state, lang, w, h).contains("comment-19"));
                state.handle(KeyCode::Up);
                assert_eq!(state.selected, 18);
                assert!(!rendered.contains('\x1b'));
            }
        }
    }
    #[test]
    fn missing_and_isolated_entity_do_not_invent_nodes_or_degree() {
        for found in [false, true] {
            for lang in [Language::En, Language::PtBr] {
                let mut snapshot = sample(0);
                snapshot.found = found;
                let mut state = State::new(snapshot);
                let output = text(&mut state, lang, 80, 24);
                assert!(output.contains(if found {
                    lang.choose("Sem relações deste tipo", "No matching outgoing")
                } else {
                    lang.choose("Entidade não encontrada", "Entity not found")
                }));
                assert!(!output.contains("comment-"));
                assert!(output.contains("0"));
            }
        }
        let mut state = State::new(sample(20));
        for (w, h) in [(1, 1), (40, 10)] {
            let _ = text(&mut state, Language::En, w, h);
        }
    }
    #[test]
    fn failed_refresh_keeps_age_rows_selection_then_recovery_preserves_identity() {
        let mut state = State::new(sample(20));
        state.selected = 19;
        state.sampled = Instant::now() - Duration::from_secs(42);
        let old = state.sampled;
        state.refresh(Err("Forbidden\x1b[31m".into()));
        assert_eq!(state.sampled, old);
        assert_eq!(state.selected, 19);
        let output = text(&mut state, Language::En, 80, 24);
        for expected in ["STALE", "42s", "Forbidden", "comment-19"] {
            assert!(output.contains(expected), "{expected}: {output}");
        }
        let mut reordered = sample(20);
        reordered.neighbors.reverse();
        state.refresh(Ok(reordered));
        assert_eq!(state.selected, 0);
        assert!(state.error.is_none());
        assert!(state.sampled > old);
        state.refresh(Ok(sample(0)));
        assert_eq!(state.selected, 0);
    }
}
