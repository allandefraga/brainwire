//! A read-only server schema snapshot. Transport stays in the CLI crate.
use crate::overview::{panel, terminal_text};
use crate::*;
use ratatui::{
    layout::{Constraint, Layout},
    text::Line,
    widgets::{Paragraph, Wrap},
    Frame,
};
use std::time::Instant;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OntologySnapshot {
    pub ontology: OntologySummary,
    pub version: u32,
    pub indexes_ready: bool,
}

pub(crate) struct State {
    pub(crate) snapshot: OntologySnapshot,
    pub(crate) selected: usize,
    pub(crate) scroll: u16,
    pub(crate) inspect: bool,
    pub(crate) sampled: Instant,
    pub(crate) error: Option<String>,
}

impl State {
    pub(crate) fn new(snapshot: OntologySnapshot) -> Self {
        Self {
            snapshot,
            selected: 0,
            scroll: 0,
            inspect: false,
            sampled: Instant::now(),
            error: None,
        }
    }

    pub(crate) fn refresh(&mut self, result: Result<OntologySnapshot, String>) {
        match result {
            Ok(snapshot) => {
                let label = self
                    .snapshot
                    .ontology
                    .nodes
                    .get(self.selected)
                    .map(|n| &n.label);
                self.selected = label
                    .and_then(|label| {
                        snapshot
                            .ontology
                            .nodes
                            .iter()
                            .position(|n| &n.label == label)
                    })
                    .unwrap_or(0);
                self.snapshot = snapshot;
                self.scroll = 0;
                self.sampled = Instant::now();
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }
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
        Constraint::Length(if state.error.is_some() { 4 } else { 3 }),
        Constraint::Min(0),
        Constraint::Length(3),
    ])
    .split(area);
    let title = format!(
        " BRAINWIRE / {} / {} @ {}",
        lang.choose("ONTOLOGIA", "ONTOLOGY"),
        terminal_text(graph),
        terminal_text(address)
    );
    frame.render_widget(
        Paragraph::new(title).block(panel("", GREEN, no_color)),
        areas[0],
    );
    let indexed = state
        .snapshot
        .ontology
        .nodes
        .iter()
        .any(|n| !n.indexed_fields.is_empty());
    let indexes = if !indexed {
        lang.choose("sem índices de nós", "no declared node indexes")
    } else if state.snapshot.indexes_ready {
        lang.choose("índices prontos (nós)", "node indexes ready")
    } else {
        lang.choose("índices pendentes (nós)", "node indexes pending")
    };
    let mut status = format!(
        " {} • schema v{} • {} {}s • {}",
        lang.choose("SNAPSHOT DO SERVIDOR", "SERVER SNAPSHOT"),
        state.snapshot.version,
        lang.choose("idade", "age"),
        state.sampled.elapsed().as_secs(),
        indexes
    );
    let status_title = if let Some(error) = &state.error {
        status.push_str(&format!(
            "\n {}",
            terminal_text(error).chars().take(240).collect::<String>()
        ));
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
            .block(panel(
                status_title,
                if state.error.is_some() { AMBER } else { CYAN },
                no_color,
            )),
        areas[1],
    );
    crate::ontology::render(
        frame,
        areas[2],
        &state.snapshot.ontology,
        state.selected,
        &mut state.scroll,
        state.inspect,
        false,
        lang,
        no_color,
    );
    frame.render_widget(
        Paragraph::new(Line::raw(lang.choose(
            " ↑↓ tipo • Enter/F1 detalhes • PgUp/Dn rola • r atualiza • q sai",
            " ↑↓ type • Enter/F1 inspect • PgUp/Dn scroll • r refresh • q quit",
        )))
        .block(panel("", GREEN, no_color)),
        areas[3],
    );
}

/// Each fetch replaces one complete schema snapshot; errors retain and mark the old one.
pub fn run_connected_ontology(
    options: &Options,
    address: &str,
    graph: &str,
    mut fetch: impl FnMut() -> Result<OntologySnapshot, String>,
) -> Result<(), String> {
    let lang = Language::parse(&options.lang)?;
    if options.view != "ontology" {
        return Err("--connect currently requires --view ontology".into());
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
            .draw(|frame| render(frame, &mut state, address, graph, lang, no_color))
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
            .draw(|frame| render(frame, &mut state, address, graph, lang, no_color))
            .map_err(|e| e.to_string())?;
        if !event::poll(Duration::from_secs(1)).map_err(|e| e.to_string())? {
            continue;
        }
        if let Event::Key(key) = event::read().map_err(|e| e.to_string())? {
            if key.kind != event::KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Char('q') => return Ok(()),
                KeyCode::Esc if state.inspect => {
                    state.inspect = false;
                    state.scroll = 0;
                }
                KeyCode::Esc => return Ok(()),
                KeyCode::Char('r') => state.refresh(fetch()),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot(labels: &[&str]) -> OntologySnapshot {
        OntologySnapshot {
            ontology: OntologySummary {
                nodes: labels
                    .iter()
                    .map(|label| OntologyNode {
                        label: label.to_string(),
                        ..Default::default()
                    })
                    .collect(),
                edges: vec![],
            },
            version: 1,
            indexes_ready: true,
        }
    }
    #[test]
    fn refresh_is_atomic_preserves_selection_and_recovers_from_stale_errors() {
        let mut state = State::new(snapshot(&["Customer", "Company"]));
        state.selected = 1;
        let sampled = state.sampled;
        state.refresh(Err("connection lost\x1b[31m".into()));
        assert_eq!(state.snapshot, snapshot(&["Customer", "Company"]));
        assert_eq!(state.sampled, sampled);
        assert!(state.error.is_some());
        for lang in [Language::En, Language::PtBr] {
            for (w, h) in [(40, 24), (60, 24), (80, 24), (160, 40), (1, 1)] {
                let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
                terminal
                    .draw(|f| render(f, &mut state, "127.0.0.1:7687", "tickets", lang, true))
                    .unwrap();
                let text: String = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|c| c.symbol())
                    .collect();
                assert!(!text.contains('\x1b'));
                if w >= 40 {
                    assert!(text.contains(lang.choose("DESATUALIZADO", "STALE")));
                }
            }
        }
        state.refresh(Ok(snapshot(&["Company", "NewType"])));
        assert_eq!(state.selected, 0);
        assert!(state.error.is_none());
        state.refresh(Ok(snapshot(&[])));
        assert_eq!(state.selected, 0);
    }
}
