//! Read-only server index declarations and equality-index readiness.
use crate::overview::{panel, terminal_text};
use crate::*;
use ratatui::{
    layout::{Constraint, Layout},
    style::Style,
    widgets::{Paragraph, Row, Table, Wrap},
    Frame,
};
use std::time::Instant;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IndexSummary {
    pub label: String,
    pub property: String,
    pub state: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IndexesSnapshot {
    pub indexes: Vec<IndexSummary>,
}
pub(crate) struct State {
    pub(crate) snapshot: IndexesSnapshot,
    pub(crate) selected: usize,
    pub(crate) sampled: Instant,
    pub(crate) error: Option<String>,
}
impl State {
    pub(crate) fn new(snapshot: IndexesSnapshot) -> Self {
        Self {
            snapshot,
            selected: 0,
            sampled: Instant::now(),
            error: None,
        }
    }
    pub(crate) fn refresh(&mut self, result: Result<IndexesSnapshot, String>) {
        match result {
            Ok(snapshot) => {
                self.selected =
                    self.snapshot
                        .indexes
                        .get(self.selected)
                        .and_then(|old| {
                            snapshot.indexes.iter().position(|new| {
                                new.label == old.label && new.property == old.property
                            })
                        })
                        .unwrap_or(0);
                self.snapshot = snapshot;
                self.sampled = Instant::now();
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }
}
// Bound display work even when a server supplies very long catalog identifiers.
fn display(text: &str) -> String {
    terminal_text(&text.chars().take(256).collect::<String>())
        .chars()
        .take(256)
        .collect()
}
fn render(
    frame: &mut Frame,
    state: &mut State,
    address: &str,
    graph: &str,
    lang: Language,
    no_color: bool,
) {
    render_at(frame, frame.area(), state, address, graph, lang, no_color);
}

pub(crate) fn render_at(
    frame: &mut Frame,
    area: ratatui::layout::Rect,
    state: &mut State,
    address: &str,
    graph: &str,
    lang: Language,
    no_color: bool,
) {
    let areas = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(if state.error.is_some() { 5 } else { 3 }),
        Constraint::Min(0),
        Constraint::Length(6),
        Constraint::Length(3),
    ])
    .split(area);
    frame.render_widget(
        Paragraph::new(format!(
            " BRAINWIRE / {} / {} @ {}",
            lang.choose("ÍNDICES", "INDEXES"),
            display(graph),
            display(address)
        ))
        .block(panel("", GREEN, no_color)),
        areas[0],
    );
    let mut status = format!(
        " {} • {} {}s • CALL db.indexes()",
        lang.choose("SNAPSHOT DO SERVIDOR", "SERVER SNAPSHOT"),
        lang.choose("idade", "age"),
        state.sampled.elapsed().as_secs()
    );
    let title = if let Some(error) = &state.error {
        status.push_str(&format!("\n {}", display(error)));
        lang.choose(
            "DESATUALIZADO • atualização falhou",
            "STALE • refresh failed",
        )
    } else {
        ""
    };
    frame.render_widget(
        Paragraph::new(status)
            .wrap(Wrap { trim: false })
            .block(panel(title, CYAN, no_color)),
        areas[1],
    );
    let capacity = usize::from(areas[2].height.saturating_sub(3)).max(1);
    let start = state.selected.saturating_sub(capacity.saturating_sub(1));
    let end = (start + capacity).min(state.snapshot.indexes.len());
    let rows = state.snapshot.indexes[start.min(end)..end]
        .iter()
        .enumerate()
        .map(|(offset, index)| {
            let selected = start + offset == state.selected;
            let status = if index.state == "scalar_equality_ready" {
                lang.choose("PRONTO", "READY")
            } else {
                lang.choose("DECLARADO", "DECLARED")
            };
            Row::new(vec![
                format!(
                    "{} {}",
                    if selected { ">" } else { " " },
                    display(&index.label)
                ),
                display(&index.property),
                status.into(),
            ])
            .style(if selected && !no_color {
                Style::default().fg(GREEN)
            } else {
                Style::default()
            })
        });
    let table = Table::new(
        rows,
        [
            Constraint::Percentage(35),
            Constraint::Percentage(40),
            Constraint::Percentage(25),
        ],
    )
    .header(Row::new([
        lang.choose("TIPO", "LABEL"),
        lang.choose("PROPRIEDADE", "PROPERTY"),
        lang.choose("ESTADO", "STATE"),
    ]))
    .block(panel(
        lang.choose(
            "ÍNDICES CONFIGURADOS / SOMENTE LEITURA",
            "CONFIGURED INDEXES / READ ONLY",
        ),
        CYAN,
        no_color,
    ));
    frame.render_widget(table, areas[2]);
    let range = if state.snapshot.indexes.is_empty() {
        lang.choose(" Nenhum índice declarado.", " No indexes declared.")
            .into()
    } else {
        format!(" {}-{} / {}", start + 1, end, state.snapshot.indexes.len())
    };
    let detail=format!("{}\n{}\n{}",range,lang.choose(" PRONTO: índice de igualdade para valores escalares suportados."," READY: equality index for supported scalar values."),lang.choose(" DECLARADO: configuração somente; consultas usam varredura.\n Não comprova suporte a valores complexos ou outros tipos de índice."," DECLARED: configuration only; queries use scans.\n Does not prove support for complex values or other index types."));
    frame.render_widget(
        Paragraph::new(detail)
            .wrap(Wrap { trim: false })
            .block(panel(
                lang.choose("O QUE SIGNIFICA?", "WHAT DOES IT MEAN?"),
                GREEN,
                no_color,
            )),
        areas[3],
    );
    frame.render_widget(
        Paragraph::new(lang.choose(
            " ↑↓ índice • PgUp/Dn rola • r atualiza • q sai",
            " ↑↓ index • PgUp/Dn scroll • r refresh • q quit",
        ))
        .block(panel("", GREEN, no_color)),
        areas[4],
    );
}
/// Replace one complete sample on refresh; failures retain and mark the old sample.
pub fn run_connected_indexes(
    options: &Options,
    address: &str,
    graph: &str,
    mut fetch: impl FnMut() -> Result<IndexesSnapshot, String>,
) -> Result<(), String> {
    let lang = Language::parse(&options.lang)?;
    if options.view != "indexes" {
        return Err("connected indexes requires --view indexes".into());
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
            .draw(|f| render(f, &mut state, address, graph, lang, no_color))
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
            .draw(|f| render(f, &mut state, address, graph, lang, no_color))
            .map_err(|e| e.to_string())?;
        if !event::poll(Duration::from_secs(1)).map_err(|e| e.to_string())? {
            continue;
        }
        if let Event::Key(key) = event::read().map_err(|e| e.to_string())? {
            if key.kind != event::KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Char('r') => state.refresh(fetch()),
                KeyCode::Up => state.selected = state.selected.saturating_sub(1),
                KeyCode::Down => {
                    state.selected =
                        (state.selected + 1).min(state.snapshot.indexes.len().saturating_sub(1))
                }
                KeyCode::PageUp => state.selected = state.selected.saturating_sub(5),
                KeyCode::PageDown => {
                    state.selected =
                        (state.selected + 5).min(state.snapshot.indexes.len().saturating_sub(1))
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(count: usize) -> IndexesSnapshot {
        IndexesSnapshot {
            indexes: (0..count)
                .map(|i| IndexSummary {
                    label: format!("Type{i}"),
                    property: "name".into(),
                    state: if i % 2 == 0 {
                        "scalar_equality_ready".into()
                    } else {
                        "declared_only".into()
                    },
                })
                .collect(),
        }
    }
    fn text(state: &mut State, lang: Language, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| render(f, state, "server\x1b[31m", "graph\n", lang, true))
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
    fn bilingual_narrow_and_stale_keep_explanations_and_escape_untrusted_text() {
        for lang in [Language::En, Language::PtBr] {
            for (w, h) in [(80, 24), (120, 36), (160, 48)] {
                let mut state = State::new(sample(3));
                for stale in [false, true] {
                    if stale {
                        state.refresh(Err("failed\x1b[31m".into()));
                    }
                    let rendered = text(&mut state, lang, w, h);
                    for expected in [
                        "Type0",
                        "Type1",
                        "Type2",
                        "1-3 / 3",
                        lang.choose("PRONTO", "READY"),
                        lang.choose("DECLARADO", "DECLARED"),
                        lang.choose("valores escalares", "scalar values"),
                        lang.choose("valores complexos", "complex values"),
                    ] {
                        assert!(
                            rendered.contains(expected),
                            "{w}x{h} {expected}: {rendered}"
                        );
                    }
                    assert!(!rendered.contains('\x1b'));
                    if stale {
                        assert!(rendered.contains(lang.choose("DESATUALIZADO", "STALE")));
                    }
                }
            }
        }
        assert!(display(&"\x1b".repeat(2000)).len() <= 256);
        let mut state = State::new(sample(1));
        state.snapshot.indexes[0].label = "bad\x1b[31m".into();
        assert!(!text(&mut state, Language::En, 80, 24).contains('\x1b'));
        for (w, h) in [(40, 24), (1, 1)] {
            let _ = text(&mut state, Language::En, w, h);
        }
    }
    #[test]
    fn refresh_preserves_selected_identity_and_stale_age_then_recovers() {
        let mut state = State::new(sample(200));
        state.selected = 199;
        state.sampled = Instant::now() - Duration::from_secs(42);
        let old = state.sampled;
        state.refresh(Err("offline".into()));
        assert_eq!(state.snapshot, sample(200));
        assert_eq!(state.sampled, old);
        let rendered = text(&mut state, Language::En, 80, 24);
        assert!(rendered.contains("Type199"));
        assert!(rendered.contains("/ 200"));
        assert!(rendered.contains("42s"));
        let mut reordered = sample(200);
        reordered.indexes.reverse();
        state.refresh(Ok(reordered));
        assert_eq!(state.selected, 0);
        assert!(state.error.is_none());
        assert!(state.sampled > old);
        state.refresh(Ok(IndexesSnapshot::default()));
        assert_eq!(state.selected, 0);
        assert!(text(&mut state, Language::En, 80, 24).contains("No indexes declared"));
    }
}
