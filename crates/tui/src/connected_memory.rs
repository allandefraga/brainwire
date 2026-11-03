//! Read-only server query-pool telemetry. Transport stays in the CLI crate.
use crate::overview::{panel, terminal_text};
use crate::*;
use ratatui::{
    layout::{Constraint, Layout},
    style::Style,
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame,
};
use std::time::Instant;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MemorySnapshot {
    pub pool_limit_bytes: Option<u64>,
    pub pool_reserved_bytes: Option<u64>,
    pub pool_peak_bytes: Option<u64>,
    pub pool_rejections: Option<u64>,
}

pub(crate) struct State {
    pub(crate) snapshot: MemorySnapshot,
    pub(crate) sampled: Instant,
    pub(crate) error: Option<String>,
}
impl State {
    pub(crate) fn new(snapshot: MemorySnapshot) -> Self {
        Self {
            snapshot,
            sampled: Instant::now(),
            error: None,
        }
    }
    pub(crate) fn refresh(&mut self, result: Result<MemorySnapshot, String>) {
        match result {
            Ok(snapshot) => {
                self.snapshot = snapshot;
                self.sampled = Instant::now();
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }
}

fn compact_byte_counter(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let (unit, divisor) = if bytes < (1 << 20) {
        ("KiB", 1 << 10)
    } else if bytes < (1 << 30) {
        ("MiB", 1 << 20)
    } else {
        ("GiB", 1 << 30)
    };
    format!("{:.2} {unit}", bytes as f64 / divisor as f64)
}

fn byte_counter(bytes: u64) -> String {
    let compact = compact_byte_counter(bytes);
    if bytes < 1024 {
        compact
    } else {
        format!("{compact} ({bytes} B)")
    }
}

fn pool_budget(snapshot: &MemorySnapshot, lang: Language) -> (Option<String>, String) {
    let (Some(limit), Some(reserved)) = (snapshot.pool_limit_bytes, snapshot.pool_reserved_bytes)
    else {
        return (
            None,
            lang.choose(
                "orçamento restante desconhecido",
                "remaining budget unknown",
            )
            .into(),
        );
    };
    if limit == 0 {
        return (
            None,
            lang.choose(
                "orçamento restante desconhecido",
                "remaining budget unknown",
            )
            .into(),
        );
    }
    let percent = reserved as f64 / limit as f64 * 100.0;
    let utilization = if percent < 10_000.0 {
        format!("{percent:.1}%")
    } else {
        format!("{percent:.1e}%")
    };
    let remaining = if let Some(bytes) = limit.checked_sub(reserved) {
        format!(
            "{} {}",
            lang.choose("orçamento restante", "remaining budget"),
            compact_byte_counter(bytes)
        )
    } else {
        format!(
            "{} {}",
            lang.choose("acima do limite em", "over limit by"),
            compact_byte_counter(reserved - limit)
        )
    };
    (Some(utilization), remaining)
}

fn render(
    frame: &mut Frame,
    state: &State,
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
    state: &State,
    address: &str,
    graph: &str,
    lang: Language,
    no_color: bool,
) {
    let areas = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(if state.error.is_some() { 5 } else { 3 }),
        Constraint::Min(0),
        Constraint::Length(3),
    ])
    .split(area);
    frame.render_widget(
        Paragraph::new(format!(
            " BRAINWIRE / {} / {} @ {}",
            lang.choose("MEMÓRIA", "MEMORY"),
            terminal_text(graph),
            terminal_text(address)
        ))
        .block(panel("", GREEN, no_color)),
        areas[0],
    );
    let mut status = format!(
        " {} • {} {}s • CALL db.memory()",
        lang.choose("SNAPSHOT DO SERVIDOR", "SERVER SNAPSHOT"),
        lang.choose("idade", "age"),
        state.sampled.elapsed().as_secs()
    );
    let title = if let Some(error) = &state.error {
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
            .block(panel(title, CYAN, no_color)),
        areas[1],
    );
    let unknown = lang.choose("desconhecido", "unknown");
    let values = [
        state.snapshot.pool_limit_bytes,
        state.snapshot.pool_reserved_bytes,
        state.snapshot.pool_peak_bytes,
        state.snapshot.pool_rejections,
    ];
    let labels = [
        lang.choose("LIMITE", "LIMIT"),
        lang.choose("RESERVADO", "RESERVED"),
        lang.choose("PICO", "PEAK"),
        lang.choose("REJEIÇÕES", "REJECTIONS"),
    ];
    let (utilization, remaining) = pool_budget(&state.snapshot, lang);
    let reserved_explanation = format!(
        "{}; {}.",
        lang.choose("Inclui consulta desta amostra", "Includes sampling query"),
        remaining
    );
    let explanations = [
        lang.choose(
            "Limite configurado de reservas do pool compartilhado de consultas.",
            "Configured shared query reservation limit.",
        ),
        reserved_explanation.as_str(),
        lang.choose(
            "Maior reserva contabilizada durante a vida deste servidor.",
            "Peak accounted reservations over this server's lifetime.",
        ),
        lang.choose(
            "Reservas rejeitadas durante a vida deste servidor.",
            "Rejected reservations over this server's lifetime.",
        ),
    ];
    let mut lines = vec![
        Line::raw(lang.choose(
            " Pool de consultas compartilhado por todos os grafos do servidor.",
            " Query pool shared by all graphs on this server.",
        )),
        Line::raw(""),
    ];
    for i in 0..4 {
        let mut value = values[i]
            .map(|v| {
                if i == 3 {
                    v.to_string()
                } else {
                    byte_counter(v)
                }
            })
            .unwrap_or_else(|| unknown.into());
        if i == 1 {
            if let Some(utilization) = &utilization {
                value.push_str(&format!(" • {utilization}"));
            }
        }
        lines.push(Line::from(vec![
            Span::raw(format!(" {}  ", labels[i])),
            Span::styled(
                value,
                if no_color {
                    Style::default()
                } else {
                    Style::default().fg(GREEN)
                },
            ),
        ]));
        lines.push(Line::raw(format!(" {}", explanations[i])));
    }
    lines.push(Line::raw(lang.choose(
        " Contabilidade de reservas; não é RSS, RAM livre/total ou cache.",
        " Reservation accounting; not OS RSS, free RAM, total RAM or cache.",
    )));
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(panel(
                lang.choose(
                    "POOL DE CONSULTAS / SOMENTE LEITURA",
                    "QUERY POOL / READ ONLY",
                ),
                CYAN,
                no_color,
            )),
        areas[2],
    );
    frame.render_widget(
        Paragraph::new(lang.choose(" r atualiza • q sai", " r refresh • q quit"))
            .block(panel("", GREEN, no_color)),
        areas[3],
    );
}

/// Replace one complete sample on refresh; failures retain and mark the old sample.
pub fn run_connected_memory(
    options: &Options,
    address: &str,
    graph: &str,
    mut fetch: impl FnMut() -> Result<MemorySnapshot, String>,
) -> Result<(), String> {
    let lang = Language::parse(&options.lang)?;
    if options.view != "memory" {
        return Err("connected memory requires --view memory".into());
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
            .draw(|f| render(f, &state, address, graph, lang, no_color))
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
            .draw(|f| render(f, &state, address, graph, lang, no_color))
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
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> MemorySnapshot {
        MemorySnapshot {
            pool_limit_bytes: Some(1000),
            pool_reserved_bytes: Some(10),
            pool_peak_bytes: Some(20),
            pool_rejections: Some(0),
        }
    }
    fn text(state: &State, lang: Language, w: u16, h: u16) -> String {
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
    fn refresh_retains_stale_data_and_age_then_recovers() {
        let mut state = State::new(sample());
        state.sampled = Instant::now() - Duration::from_secs(42);
        let sampled = state.sampled;
        state.refresh(Err("offline\x1b[31m".into()));
        assert_eq!(state.snapshot, sample());
        assert_eq!(state.sampled, sampled);
        for lang in [Language::En, Language::PtBr] {
            let rendered = text(&state, lang, 80, 24);
            for expected in [
                lang.choose("DESATUALIZADO", "STALE"),
                "42s",
                "1000 B",
                "10 B",
                "20 B",
                "RSS",
                "cache",
                lang.choose("REJEIÇÕES", "REJECTIONS"),
            ] {
                assert!(rendered.contains(expected), "{expected}: {rendered}");
            }
            assert!(!rendered.contains('\x1b'));
        }
        state.refresh(Ok(MemorySnapshot::default()));
        assert!(state.error.is_none());
        assert!(state.sampled > sampled);
        assert_eq!(
            text(&state, Language::En, 80, 24)
                .matches("unknown")
                .count(),
            5
        );
    }
    #[test]
    fn bilingual_narrow_render_keeps_all_counters_and_scope() {
        for lang in [Language::En, Language::PtBr] {
            for (w, h) in [(80, 24), (120, 36), (160, 48), (240, 80)] {
                let rendered = text(&State::new(sample()), lang, w, h);
                for expected in [
                    "1000 B",
                    "10 B",
                    "20 B",
                    lang.choose("REJEIÇÕES", "REJECTIONS"),
                    lang.choose("todos os grafos", "all graphs"),
                    lang.choose("consulta desta amostra", "sampling query"),
                    "RSS",
                    "cache",
                ] {
                    assert!(rendered.contains(expected), "{w}x{h}: {expected}");
                }
                assert!(!rendered.contains('\x1b'));
            }
        }
        for (w, h) in [(40, 24), (1, 1)] {
            let _ = text(&State::new(sample()), Language::En, w, h);
        }
    }
    #[test]
    fn snapshot_options_are_validated_before_fetch_and_theme_uses_green_cyan() {
        let options = Options {
            demo: false,
            lang: "en".into(),
            no_anim: true,
            snapshot: true,
            view: "memory".into(),
            width: 79,
            height: 24,
        };
        assert!(run_connected_memory(&options, "server", "graph", || panic!(
            "must validate before fetch"
        ))
        .unwrap_err()
        .contains("snapshot size"));
        let state = State::new(sample());
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| render(f, &state, "server", "graph", Language::En, false))
            .unwrap();
        let cells = &terminal.backend().buffer().content;
        assert!(cells.iter().any(|c| c.fg == GREEN));
        assert!(cells.iter().any(|c| c.fg == CYAN));
    }
    #[test]
    fn pool_utilization_and_remaining_budget_are_scoped_and_handle_unknowns() {
        let snapshot = MemorySnapshot {
            pool_limit_bytes: Some(1000),
            pool_reserved_bytes: Some(250),
            pool_peak_bytes: Some(400),
            pool_rejections: Some(2),
        };
        for lang in [Language::En, Language::PtBr] {
            let mut state = State::new(snapshot.clone());
            for stale in [false, true] {
                if stale {
                    state.refresh(Err("offline".into()));
                }
                let rendered = text(&state, lang, 80, 24);
                assert!(rendered.contains("25.0%"), "{rendered}");
                assert!(rendered.contains("750 B"), "{rendered}");
                assert!(
                    rendered.contains(lang.choose("RAM livre", "free RAM")),
                    "{rendered}"
                );
                assert!(
                    rendered.contains(lang.choose("consulta desta amostra", "sampling query")),
                    "{rendered}"
                );
                assert!(
                    rendered.contains("RSS") && rendered.contains("cache"),
                    "{rendered}"
                );
            }
        }
        let over = MemorySnapshot {
            pool_reserved_bytes: Some(1200),
            ..snapshot.clone()
        };
        let rendered = text(&State::new(over), Language::En, 80, 24);
        assert!(
            rendered.contains("120.0%") && rendered.contains("over limit by 200 B"),
            "{rendered}"
        );
        for limit in [None, Some(0)] {
            let unknown = MemorySnapshot {
                pool_limit_bytes: limit,
                ..snapshot.clone()
            };
            let rendered = text(&State::new(unknown), Language::En, 80, 24);
            assert!(!rendered.contains('%'), "{rendered}");
            assert!(rendered.contains("remaining budget unknown"), "{rendered}");
        }
        let extreme = MemorySnapshot {
            pool_limit_bytes: Some(1),
            pool_reserved_bytes: Some(u64::MAX),
            ..snapshot.clone()
        };
        let mut state = State::new(extreme);
        state.refresh(Err("offline".into()));
        for lang in [Language::En, Language::PtBr] {
            let rendered = text(&state, lang, 80, 24);
            assert!(rendered.contains("1.8e21%"), "{rendered}");
            assert!(
                rendered.contains("RSS") && rendered.contains("cache"),
                "{rendered}"
            );
        }
        let unknown = MemorySnapshot {
            pool_reserved_bytes: None,
            ..snapshot
        };
        let rendered = text(&State::new(unknown), Language::En, 80, 24);
        assert!(!rendered.contains('%'), "{rendered}");
        assert!(rendered.contains("remaining budget unknown"), "{rendered}");
    }

    #[test]
    fn human_byte_counters_keep_exact_values_in_compact_bilingual_render() {
        assert_eq!(byte_counter(1023), "1023 B");
        assert_eq!(byte_counter(1024), "1.00 KiB (1024 B)");
        assert_eq!(byte_counter(1 << 20), "1.00 MiB (1048576 B)");
        assert_eq!(byte_counter(1 << 30), "1.00 GiB (1073741824 B)");
        let snapshot = MemorySnapshot {
            pool_limit_bytes: Some(64 << 20),
            pool_reserved_bytes: Some(1536),
            pool_peak_bytes: Some(2 << 30),
            pool_rejections: Some(1024),
        };
        for lang in [Language::En, Language::PtBr] {
            let mut state = State::new(snapshot.clone());
            for stale in [false, true] {
                if stale {
                    state.refresh(Err("offline".into()));
                }
                let rendered = text(&state, lang, 80, 24);
                for expected in [
                    "64.00 MiB (67108864 B)",
                    "1.50 KiB (1536 B)",
                    "2.00 GiB (2147483648 B)",
                    "RSS",
                    "cache",
                ] {
                    assert!(rendered.contains(expected), "{expected}: {rendered}");
                }
                assert!(rendered.contains(lang.choose("REJEIÇÕES  1024", "REJECTIONS  1024")));
                assert!(
                    !rendered.contains("1.00 KiB (1024 B)"),
                    "rejections must remain a count"
                );
            }
        }
    }
}
