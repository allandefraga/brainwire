//! Bounded outgoing relationships for one configured entity; no graph writes.
use crate::overview::{panel, terminal_text};
use crate::*;
use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{
        canvas::{Canvas, Line as CanvasLine},
        Clear, Paragraph, Wrap,
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
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = last,
            _ => {}
        }
    }
}
fn display(text: &str) -> String {
    terminal_text(&text.chars().take(256).collect::<String>())
}
// Canvas coordinates use terminal cells, so labels can be bounded without
// relying on the byte length of identifiers returned by the server.
fn fit(text: &str, width: usize) -> String {
    let text = display(text);
    if Span::raw(&text).width() <= width {
        return text;
    }
    let mut result = String::new();
    let mut used = 0;
    for c in text.chars() {
        let cells = Span::raw(c.to_string()).width();
        if used + cells + 1 > width {
            break;
        }
        result.push(c);
        used += cells;
    }
    if width > 0 {
        result.push('…');
    }
    result
}

// Wings remain behind the destination so both canvas sides preserve direction.
fn arrow_wings(tip_x: f64, tip_y: f64, left: bool) -> [(f64, f64); 2] {
    let back = if left { 1.8 } else { -1.8 };
    [(tip_x + back, tip_y - 0.7), (tip_x + back, tip_y + 0.7)]
}

// Duplicate relationship rows share a single target node on the canvas.
fn targets(snapshot: &GraphSnapshot) -> Vec<(&str, usize)> {
    let mut targets: Vec<(&str, usize)> = Vec::new();
    for id in snapshot.neighbors.iter().take(20) {
        if let Some((_, count)) = targets.iter_mut().find(|(target, _)| *target == id) {
            *count += 1;
        } else {
            targets.push((id, 1));
        }
    }
    targets
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
    let color = if no_color { Color::Reset } else { ACCENT };
    let cyan = if no_color { Color::Reset } else { CYAN };
    let areas = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(if state.error.is_some() { 6 } else { 5 }),
        Constraint::Min(0),
        Constraint::Length(3),
    ])
    .split(area);
    frame.render_widget(
        Paragraph::new(format!(
            " ●─●─● BRAINWIRE / {} / {} @ {}",
            lang.choose("GRAFO", "GRAPH"),
            display(graph),
            display(address)
        ))
        .block(panel("", ACCENT, no_color)),
        areas[0],
    );
    let count = if state.snapshot.found {
        state.snapshot.neighbors.len().min(20)
    } else {
        0
    };
    let more = state.snapshot.has_more || state.snapshot.neighbors.len() > 20;
    let target_ids = if state.snapshot.found {
        targets(&state.snapshot).len()
    } else {
        0
    };
    let mut status = format!(
        " {} • {} {}s{}\n {} {} / {} {}\n {}",
        lang.choose("SNAPSHOT DO SERVIDOR", "SERVER SNAPSHOT"),
        lang.choose("idade", "age"),
        state.sampled.elapsed().as_secs(),
        if more {
            lang.choose(" • MAIS DISPONÍVEIS", " • MORE AVAILABLE")
        } else {
            ""
        },
        count,
        lang.choose("relações de saída", "outgoing rows"),
        target_ids,
        lang.choose("IDs de destino distintos", "distinct target IDs"),
        lang.choose(
            "Até 20 relações; não é o grau total. Um tipo/direção por amostra.",
            "Up to 20 relationships; not total degree. One type/direction per sample."
        )
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
                    lang.choose("DADOS REAIS • SOMENTE LEITURA", "REAL DATA • READ ONLY")
                },
                if state.error.is_some() { AMBER } else { CYAN },
                no_color,
            )),
        areas[1],
    );
    let wide = areas[2].width >= 105;
    let body = if wide {
        Layout::horizontal([Constraint::Percentage(70), Constraint::Percentage(30)]).split(areas[2])
    } else {
        Layout::vertical([
            Constraint::Length(7.min(areas[2].height / 2 + 1)),
            Constraint::Min(0),
        ])
        .split(areas[2])
    };
    let unconfigured = state.snapshot.entity_id.is_empty();
    let selected_id = state
        .snapshot
        .neighbors
        .get(state.selected)
        .filter(|_| count > 0);
    if !state.snapshot.found {
        frame.render_widget(
            Paragraph::new(if unconfigured {
                lang.choose(
                    " Amostra não configurada.\n Nenhum dado foi consultado.",
                    " Sample not configured.\n No graph data was queried.",
                )
            } else {
                lang.choose(
                    " Entidade não encontrada.\n Nenhum nó foi criado.",
                    " Entity not found.\n No node was created.",
                )
            })
            .wrap(Wrap { trim: false })
            .block(panel(
                lang.choose("VIZINHANÇA", "NEIGHBORHOOD"),
                ACCENT,
                no_color,
            )),
            body[0],
        );
    } else if !wide || body[0].height < 10 {
        let available = usize::from(body[0].width.saturating_sub(5));
        let mut lines = vec![
            Line::styled(
                format!(" ● {}", fit(&state.snapshot.node_label, available)),
                Style::default().fg(cyan),
            ),
            Line::styled(
                format!("   {}", fit(&state.snapshot.entity_id, available)),
                Style::default().fg(color),
            ),
        ];
        if let Some(id) = selected_id {
            let self_link = state.snapshot.node_label == state.snapshot.target_label
                && id == &state.snapshot.entity_id;
            lines.push(Line::styled(
                format!(
                    " {} {} ─▶",
                    if self_link { "↺" } else { "└─" },
                    fit(&state.snapshot.edge_type, available.saturating_sub(8))
                ),
                Style::default().fg(cyan),
            ));
            lines.push(Line::styled(
                format!(" ○ {}", fit(&state.snapshot.target_label, available)),
                Style::default().fg(cyan),
            ));
            lines.push(Line::styled(
                format!("   {}", fit(id, available)),
                Style::default().fg(color),
            ));
        } else {
            lines.push(Line::raw(lang.choose(
                " Sem relações deste tipo na amostra.",
                " No matching outgoing relationships.",
            )));
        }
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }).block(
                panel("", ACCENT, no_color).title(format!(
                    "{} {}/{} • {}",
                    lang.choose("RELAÇÃO", "LINK"),
                    if selected_id.is_some() {
                        state.selected + 1
                    } else {
                        0
                    },
                    count,
                    lang.choose("ORIGEM → DESTINO", "SOURCE → TARGET")
                )),
            ),
            body[0],
        );
    } else {
        let grouped = targets(&state.snapshot);
        let is_self = |id: &str| {
            state.snapshot.node_label == state.snapshot.target_label
                && id == state.snapshot.entity_id
        };
        let self_count = grouped
            .iter()
            .find(|(id, _)| is_self(id))
            .map_or(0, |(_, n)| *n);
        let visible: Vec<_> = grouped.iter().filter(|(id, _)| !is_self(id)).collect();
        let width = body[0].width.saturating_sub(2);
        let height = body[0].height.saturating_sub(2);
        let slots = usize::from(height.saturating_sub(4) / 5).clamp(1, 4);
        let capacity = slots * 2;
        let selected_group =
            selected_id.and_then(|selected| visible.iter().position(|(id, _)| *id == selected));
        let start = selected_group
            .unwrap_or(0)
            .saturating_sub(capacity.saturating_sub(1));
        let end = (start + capacity).min(visible.len());
        let node_width = (width / 4).max(1);
        let source_x = width.saturating_sub(node_width) / 2;
        let source_top = height.saturating_sub(3) / 2;
        let source_y = f64::from(height.saturating_sub(source_top + 2));
        let right_x = width.saturating_sub(node_width + 1);
        let title = format!(
            " {} • {}–{} / {}",
            lang.choose("DESTINOS REAIS", "REAL TARGETS"),
            if visible.is_empty() { 0 } else { start + 1 },
            end,
            visible.len()
        );
        let target_top = |position: usize| -> u16 {
            let row = position / 2;
            let occupied = (end - start).div_ceil(2).max(1);
            let stride = usize::from(height.saturating_sub(4)) / occupied;
            (2 + row * stride) as u16
        };
        frame.render_widget(
            Canvas::default()
                .x_bounds([0.0, f64::from(width.max(1))])
                .y_bounds([0.0, f64::from(height.max(1))])
                .block(panel("", ACCENT, no_color).title(title))
                .paint(|ctx| {
                    for (position, (id, _)) in visible.iter().skip(start).take(capacity).enumerate()
                    {
                        let left = position % 2 == 0;
                        let y = f64::from(height.saturating_sub(target_top(position) + 1));
                        let selected = selected_id.is_some_and(|selected| selected == *id);
                        let edge_color = if selected {
                            color
                        } else if no_color {
                            Color::Reset
                        } else {
                            Color::Rgb(69, 91, 128)
                        };
                        let from_x = f64::from(if left {
                            source_x
                        } else {
                            source_x + node_width
                        });
                        let to_x = f64::from(if left {
                            node_width + 1
                        } else {
                            right_x.saturating_sub(1)
                        });
                        ctx.draw(&CanvasLine {
                            x1: from_x,
                            y1: source_y,
                            x2: to_x,
                            y2: y,
                            color: edge_color,
                        });
                        for (wing_x, wing_y) in arrow_wings(to_x, y, left) {
                            ctx.draw(&CanvasLine {
                                x1: to_x,
                                y1: y,
                                x2: wing_x,
                                y2: wing_y,
                                color: edge_color,
                            });
                        }
                    }
                    ctx.print(
                        2.0,
                        f64::from(height) - 0.3,
                        Span::styled(
                            fit(
                                &state.snapshot.edge_type,
                                usize::from(width.saturating_sub(4)),
                            ),
                            Style::default().fg(cyan),
                        ),
                    );
                    if self_count > 0 {
                        let left = f64::from(source_x + node_width / 3);
                        let right = f64::from(source_x + node_width * 2 / 3);
                        for (x1, y1, x2, y2) in [
                            (left, source_y + 2.0, left, source_y + 4.0),
                            (left, source_y + 4.0, right, source_y + 4.0),
                            (right, source_y + 4.0, right, source_y + 2.0),
                            (right, source_y + 2.0, right - 1.0, source_y + 3.0),
                        ] {
                            ctx.draw(&CanvasLine {
                                x1,
                                y1,
                                x2,
                                y2,
                                color,
                            });
                        }
                        ctx.print(
                            f64::from(source_x),
                            (source_y + 5.0).min(f64::from(height.saturating_sub(1))),
                            Span::styled(
                                format!(
                                    "↺ {} ×{self_count}",
                                    lang.choose("autorrelação", "self link")
                                ),
                                Style::default().fg(color),
                            ),
                        );
                    }
                    if count == 0 {
                        ctx.print(
                            2.0,
                            1.0,
                            Span::styled(
                                lang.choose(
                                    "Sem relações deste tipo na amostra.",
                                    "No matching outgoing relationships.",
                                ),
                                Style::default().fg(cyan),
                            ),
                        );
                    }
                }),
            body[0],
        );
        let source = Rect::new(
            body[0].x + 1 + source_x,
            body[0].y + 1 + source_top,
            node_width,
            3,
        );
        frame.render_widget(Clear, source);
        frame.render_widget(
            Paragraph::new(fit(
                &state.snapshot.entity_id,
                usize::from(node_width.saturating_sub(2)),
            ))
            .style(Style::default().fg(color))
            .block(panel("", ACCENT, no_color).title(format!(
                "● {}",
                fit(
                    &state.snapshot.node_label,
                    usize::from(node_width.saturating_sub(4))
                )
            ))),
            source,
        );
        for (position, (id, multiplicity)) in visible.iter().skip(start).take(capacity).enumerate()
        {
            let selected = selected_id.is_some_and(|selected| selected == *id);
            let target = Rect::new(
                body[0].x + 1 + if position % 2 == 0 { 1 } else { right_x },
                body[0].y + 1 + target_top(position),
                node_width,
                2,
            );
            let identity = format!(
                "{}{}",
                if *multiplicity > 1 {
                    format!("×{multiplicity} ")
                } else {
                    String::new()
                },
                display(id)
            );
            frame.render_widget(Clear, target);
            frame.render_widget(
                Paragraph::new(vec![
                    Line::styled(
                        if position % 2 == 0 {
                            format!(
                                "{} {}",
                                fit(
                                    &state.snapshot.target_label,
                                    usize::from(node_width.saturating_sub(3))
                                ),
                                if selected { "◉" } else { "○" }
                            )
                        } else {
                            format!(
                                "{} {}",
                                if selected { "◉" } else { "○" },
                                fit(
                                    &state.snapshot.target_label,
                                    usize::from(node_width.saturating_sub(3))
                                )
                            )
                        },
                        Style::default().fg(if selected { color } else { cyan }),
                    ),
                    Line::styled(
                        fit(&identity, usize::from(node_width)),
                        Style::default().fg(if selected { color } else { cyan }),
                    ),
                ])
                .alignment(if position % 2 == 0 {
                    Alignment::Right
                } else {
                    Alignment::Left
                }),
                target,
            );
        }
    }
    let sidebar = if wide {
        Layout::vertical([
            Constraint::Length(9.min(body[1].height.saturating_sub(3))),
            Constraint::Min(0),
        ])
        .split(body[1])
    } else {
        Layout::vertical([Constraint::Length(0), Constraint::Min(0)]).split(body[1])
    };
    if wide {
        let available = usize::from(sidebar[0].width.saturating_sub(5));
        let mut detail = vec![
            Line::styled(
                format!(" ● {}", fit(&state.snapshot.node_label, available)),
                Style::default().fg(cyan),
            ),
            Line::styled(
                format!("   {}", fit(&state.snapshot.entity_id, available)),
                Style::default().fg(color),
            ),
        ];
        if let Some(id) = selected_id {
            detail.push(Line::styled(
                format!(
                    " └─ {} ─▶",
                    fit(&state.snapshot.edge_type, available.saturating_sub(7))
                ),
                Style::default().fg(cyan),
            ));
            detail.push(Line::styled(
                format!(" ○ {}", fit(&state.snapshot.target_label, available)),
                Style::default().fg(cyan),
            ));
            detail.push(Line::styled(
                format!("   {}", fit(id, available)),
                Style::default().fg(color),
            ));
            detail.push(Line::raw(fit(
                &format!(
                    " {} {}/{} • {}",
                    lang.choose("Relação", "Row"),
                    state.selected + 1,
                    count,
                    lang.choose("sem propriedades", "no properties")
                ),
                usize::from(sidebar[0].width.saturating_sub(2)),
            )));
        } else {
            detail.push(Line::raw(
                lang.choose(" Sem relação selecionada.", " No selected relationship."),
            ));
        }
        frame.render_widget(
            Paragraph::new(detail).block(panel(
                lang.choose("INSPECIONAR RELAÇÃO", "LINK INSPECTOR"),
                ACCENT,
                no_color,
            )),
            sidebar[0],
        );
    }
    let capacity = usize::from(sidebar[1].height.saturating_sub(2)).max(1);
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
                        if i == state.selected { "▶" } else { " " },
                        i + 1,
                        display(id)
                    ),
                    if i == state.selected {
                        Style::default().fg(color)
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
        sidebar[1],
    );
    frame.render_widget(
        Paragraph::new(lang.choose(
            " ↑↓ seleciona • Home/End início/fim • r atualiza • q sai",
            " ↑↓ select • Home/End first/last • r refresh • q quit",
        ))
        .block(panel(
            lang.choose(
                "Arestas apontam da origem ao destino. Sem propriedades.",
                "Arrows point from source to target. No properties.",
            ),
            ACCENT,
            no_color,
        )),
        areas[3],
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
    fn sample_counts_distinguish_relationship_rows_from_target_ids_and_jump_to_edges() {
        for lang in [Language::En, Language::PtBr] {
            for (width, height) in [(80, 24), (120, 36)] {
                let mut snapshot = sample(20);
                snapshot.neighbors[1] = snapshot.neighbors[0].clone();
                let mut state = State::new(snapshot);
                state.handle(KeyCode::End);
                assert_eq!(state.selected, 19);
                let output = text(&mut state, lang, width, height);
                for expected in [
                    lang.choose("19 IDs de destino distintos", "19 distinct target IDs"),
                    "20/20",
                    "comment-19",
                    lang.choose("não é o grau total", "not total degree"),
                ] {
                    assert!(output.contains(expected), "{expected}: {output}");
                }
                state.handle(KeyCode::Home);
                assert_eq!(state.selected, 0);
                let output = text(&mut state, lang, width, height);
                assert!(output.contains("comment-0"));
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

    #[test]
    fn arrows_point_outward_on_both_sides_and_short_canvas_explains_self_links() {
        for left in [true, false] {
            let tip = if left { 20.0 } else { 80.0 };
            for (x, _) in arrow_wings(tip, 10.0, left) {
                assert!(
                    (x - tip) * (50.0 - tip) > 0.0,
                    "wing must remain behind its tip toward the source"
                );
            }
        }
        let mut snapshot = sample(1);
        snapshot.target_label = snapshot.node_label.clone();
        snapshot.neighbors = vec![snapshot.entity_id.clone()];
        for lang in [Language::En, Language::PtBr] {
            let output = text(&mut State::new(snapshot.clone()), lang, 144, 24);
            assert!(output.contains(lang.choose("autorrelação", "self link")));
        }
    }

    #[test]
    fn constellation_shows_both_sides_and_the_selected_real_identity() {
        let mut state = State::new(sample(8));
        let mut terminal = Terminal::new(TestBackend::new(180, 48)).unwrap();
        terminal
            .draw(|frame| {
                render_at(
                    frame,
                    frame.area(),
                    &mut state,
                    "localhost",
                    "demo",
                    Language::En,
                    true,
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let locate = |needle: &str| {
            (0..48)
                .find_map(|y| {
                    let line: String = (0..126).map(|x| buffer[(x, y)].symbol()).collect();
                    line.find(needle).map(|x| (x, usize::from(y)))
                })
                .unwrap_or_else(|| panic!("missing {needle}"))
        };
        let source = locate("ticket-1");
        let left = locate("comment-0");
        let right = locate("comment-1");
        assert!(
            left.0 < source.0 && right.0 > source.0,
            "nodes should surround source: {left:?} {source:?} {right:?}"
        );
        for id in 0..8 {
            locate(&format!("comment-{id}"));
        }
        state.selected = 7;
        assert!(text(&mut state, Language::En, 180, 48).contains("comment-7"));
    }

    #[test]
    fn canvas_groups_parallel_rows_and_draws_self_links_on_the_source() {
        let mut snapshot = sample(4);
        snapshot.target_label = snapshot.node_label.clone();
        snapshot.neighbors = vec![
            "ticket-1".into(),
            "ticket-2".into(),
            "ticket-2".into(),
            "ticket-3".into(),
        ];
        assert_eq!(
            targets(&snapshot),
            vec![("ticket-1", 1), ("ticket-2", 2), ("ticket-3", 1)]
        );
        for lang in [Language::En, Language::PtBr] {
            let mut state = State::new(snapshot.clone());
            let output = text(&mut state, lang, 144, 40);
            assert!(output.contains(lang.choose("autorrelação", "self link")));
            assert!(output.contains("×2 ticket-2"));
            assert_eq!(state.snapshot.neighbors.len(), 4);
        }
    }

    #[test]
    fn canvas_follows_last_selected_row_and_bounds_untrusted_wide_labels() {
        let value = format!("節点\x1b[31m\n{}", "界".repeat(200));
        for width in [0, 1, 8, 19, 40] {
            let fitted = fit(&value, width);
            assert!(Span::raw(&fitted).width() <= width);
            assert!(!fitted.contains('\x1b'));
            assert!(!fitted.contains('\n'));
        }
        for lang in [Language::En, Language::PtBr] {
            for width in [80, 120, 144] {
                let mut snapshot = sample(20);
                snapshot.entity_id = format!("source-{}", value);
                snapshot.neighbors[0] = value.clone();
                snapshot.neighbors[19] = format!("selected-{}", value);
                let mut state = State::new(snapshot);
                state.selected = 19;
                let output = text(&mut state, lang, width, 30);
                assert!(output.contains("source-"));
                assert!(output.contains("selected-"));
                assert!(output.contains("HAS_COMMENT"));
                assert!(output.contains("TicketComment"));
                assert!(output.contains(lang.choose("não é o grau total", "not total degree")));
                assert!(output.contains(lang.choose("MAIS DISPONÍVEIS", "MORE AVAILABLE")));
            }
        }
    }

    #[test]
    fn short_wide_inspector_retains_selected_target_when_fresh_or_stale() {
        for lang in [Language::En, Language::PtBr] {
            for stale in [false, true] {
                let mut snapshot = sample(20);
                snapshot.entity_id = format!("source-{}", "界".repeat(200));
                snapshot.neighbors[19] = format!("chosen-{}", "界".repeat(200));
                let mut state = State::new(snapshot);
                state.selected = 19;
                if stale {
                    state.refresh(Err("Unavailable".into()));
                }
                let output = text(&mut state, lang, 120, 24);
                for expected in ["source-", "chosen-", "HAS_COMMENT", "TicketComment"] {
                    assert!(output.contains(expected), "{stale} {expected}: {output}");
                }
            }
        }
    }

    #[test]
    fn an_unconfigured_graph_sample_is_not_reported_as_a_missing_entity() {
        for lang in [Language::En, Language::PtBr] {
            let mut state = State::new(GraphSnapshot::default());
            state.refresh(Err(
                "Graph preview needs --node-label, --entity-id, --edge-type and --target-label"
                    .into(),
            ));
            let rendered = text(&mut state, lang, 80, 24);
            assert!(
                rendered.contains(lang.choose("Amostra não configurada", "Sample not configured"))
            );
            assert!(rendered
                .contains(lang.choose("Nenhum dado foi consultado", "No graph data was queried")));
            assert!(rendered.contains("--entity-id"));
        }
    }
}
