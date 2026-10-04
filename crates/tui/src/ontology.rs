//! Read-only schema connections, independent of stored instance counts.

use crate::overview::{panel, terminal_text, text_style};
use crate::{Language, CYAN, GREEN, PALE};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::Modifier,
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OntologySummary {
    pub nodes: Vec<OntologyNode>,
    pub edges: Vec<OntologyEdge>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OntologyNode {
    pub label: String,
    pub properties: Vec<String>,
    pub indexed_fields: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OntologyEdge {
    pub name: String,
    pub source: String,
    pub target: String,
    pub properties: Vec<String>,
}

// Truncate by terminal cells, preserving Unicode boundaries and escaped controls.
fn fitted(text: &str, width: usize) -> String {
    let text = terminal_text(text);
    if Line::raw(text.as_str()).width() <= width {
        return text;
    }
    let mut result = String::new();
    for ch in text.chars() {
        let next_width = Line::raw(ch.to_string()).width();
        if Line::raw(result.as_str()).width() + next_width > width.saturating_sub(1) {
            break;
        }
        result.push(ch);
    }
    if width > 0 {
        result.push('…');
    }
    result
}

fn signature(edge: &OntologyEdge) -> String {
    format!(
        "{} -[:{}]-> {}",
        terminal_text(&edge.source),
        terminal_text(&edge.name),
        terminal_text(&edge.target)
    )
}

fn inspector(node: &OntologyNode, ontology: &OntologySummary, lang: Language) -> String {
    let mut text = format!(
        "{}\n\n{}\n",
        terminal_text(&node.label),
        lang.choose("PROPRIEDADES", "PROPERTIES")
    );
    let mut fields = |title: &str, values: &[String]| {
        text.push_str(title);
        if values.is_empty() {
            text.push_str(lang.choose("  Nenhuma declaração.\n", "  No declarations.\n"));
        } else {
            for value in values {
                text.push_str(&format!("  {}\n", terminal_text(value)));
            }
        }
    };
    fields("", &node.properties);
    fields(
        lang.choose("\nÍNDICES DECLARADOS\n", "\nDECLARED INDEXES\n"),
        &node.indexed_fields,
    );
    for (heading, outgoing) in [
        (lang.choose("SAÍDA", "OUTGOING"), true),
        (lang.choose("ENTRADA", "INCOMING"), false),
    ] {
        text.push_str(&format!("\n{heading}\n"));
        let mut found = false;
        for edge in &ontology.edges {
            if (outgoing && edge.source == node.label) || (!outgoing && edge.target == node.label) {
                found = true;
                text.push_str(&signature(edge));
                text.push('\n');
                if edge.source == edge.target {
                    text.push_str(lang.choose(
                        "  Conexão ao próprio tipo.\n",
                        "  Self-loop on this type.\n",
                    ));
                }
                if !edge.properties.is_empty() {
                    text.push_str(lang.choose("  Propriedades: ", "  Properties: "));
                    text.push_str(
                        &edge
                            .properties
                            .iter()
                            .map(|p| terminal_text(p))
                            .collect::<Vec<_>>()
                            .join(", "),
                    );
                    text.push('\n');
                }
            }
        }
        if !found {
            text.push_str(lang.choose("  Nenhuma declaração.\n", "  No declarations.\n"));
        }
    }
    text.push_str(lang.choose(
        "\nConexões permitidas pelo schema; não são registros de instâncias. Índices declarados não indicam prontidão.",
        "\nConnections allowed by the schema; these are not instance records. Declared indexes do not indicate readiness.",
    ));
    text
}

fn connections(
    node: &OntologyNode,
    ontology: &OntologySummary,
    width: usize,
    lang: Language,
    no_color: bool,
) -> Vec<Line<'static>> {
    let green = text_style(GREEN, no_color);
    let cyan = text_style(CYAN, no_color);
    let pale = text_style(PALE, no_color);
    let mut lines = vec![
        Line::styled(
            terminal_text(&node.label),
            green.add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            lang.choose(
                "Schema • conexões declaradas",
                "Schema • declared connections",
            ),
            pale,
        ),
        Line::raw(""),
    ];
    let mut found = false;
    for edge in ontology
        .edges
        .iter()
        .filter(|e| e.source == node.label || e.target == node.label)
    {
        found = true;
        lines.push(Line::styled(terminal_text(&edge.name), cyan));
        if width < 26 {
            // A text signature retains direction when boxes cannot fit.
            lines.push(Line::styled(signature(edge), green));
        } else {
            let box_width = ((width.saturating_sub(5)) / 3).clamp(4, 20);
            let gap = width.saturating_sub(box_width * 2);
            let source = fitted(&edge.source, box_width - 2);
            let target = fitted(&edge.target, box_width - 2);
            let padded = |s: &str| {
                format!(
                    "{s}{}",
                    " ".repeat((box_width - 2).saturating_sub(Line::raw(s).width()))
                )
            };
            let top = format!("┌{}┐", "─".repeat(box_width - 2));
            let bottom = format!("└{}┘", "─".repeat(box_width - 2));
            lines.push(Line::from(vec![
                Span::styled(top.clone(), green),
                Span::raw(" ".repeat(gap)),
                Span::styled(top, green),
            ]));
            let relationship = fitted(&edge.name, gap.saturating_sub(3));
            let arrow = format!(
                "─{relationship}{}>",
                "─".repeat(gap.saturating_sub(Line::raw(relationship.as_str()).width() + 2))
            );
            lines.push(Line::from(vec![
                Span::styled(format!("│{}│", padded(&source)), green),
                Span::styled(arrow, cyan),
                Span::styled(format!("│{}│", padded(&target)), green),
            ]));
            lines.push(Line::from(vec![
                Span::styled(bottom.clone(), green),
                Span::raw(" ".repeat(gap)),
                Span::styled(bottom, green),
            ]));
        }
        if edge.source == edge.target {
            lines.push(Line::styled(
                lang.choose("  ↳ próprio tipo", "  ↳ self-loop"),
                pale,
            ));
        }
        lines.push(Line::raw(""));
    }
    if !found {
        lines.push(Line::styled(
            format!("[ {} ]", terminal_text(&node.label)),
            green,
        ));
        lines.push(Line::styled(
            lang.choose(
                "Tipo isolado • sem conexões declaradas.",
                "Isolated type • no declared connections.",
            ),
            pale,
        ));
    }
    lines
}

fn scrolled(frame: &mut Frame, area: Rect, paragraph: Paragraph<'_>, scroll: u16) -> u16 {
    let width = area.width.saturating_sub(2);
    if width == 0 || area.height <= 2 {
        frame.render_widget(paragraph, area);
        return 0;
    }
    let max = paragraph
        .line_count(width)
        .saturating_sub(area.height.saturating_sub(2) as usize)
        .min(u16::MAX as usize) as u16;
    frame.render_widget(paragraph.scroll((scroll.min(max), 0)), area);
    max
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render(
    frame: &mut Frame,
    area: Rect,
    ontology: &OntologySummary,
    selected: usize,
    scroll: &mut u16,
    inspect: bool,
    lang: Language,
    no_color: bool,
) {
    if area.width == 0 || area.height == 0 {
        *scroll = 0;
        return;
    }
    let mut max_scroll = 0;
    let wide = area.width >= 100;
    let columns = if wide {
        Layout::horizontal([
            Constraint::Length(22),
            Constraint::Min(30),
            Constraint::Length(32),
        ])
        .split(area)
    } else {
        Layout::horizontal([
            Constraint::Length(area.width.min(20) / 2 + 10),
            Constraint::Min(1),
        ])
        .split(area)
    };
    let selected = selected.min(ontology.nodes.len().saturating_sub(1));
    let visible = columns[0].height.saturating_sub(2) as usize;
    let start = selected.saturating_sub(visible.saturating_sub(1));
    let mut types = ontology
        .nodes
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .map(|(i, node)| {
            Line::styled(
                format!(
                    "{} {}",
                    if i == selected { ">" } else { " " },
                    terminal_text(&node.label)
                ),
                text_style(if i == selected { GREEN } else { PALE }, no_color),
            )
        })
        .collect::<Vec<_>>();
    if ontology.nodes.is_empty() {
        types.push(Line::raw(lang.choose("Nenhum tipo.", "No types.")));
    }
    frame.render_widget(
        Paragraph::new(types)
            .style(text_style(PALE, no_color))
            .block(panel(
                lang.choose("TIPOS • ↑↓", "TYPES • ↑↓"),
                GREEN,
                no_color,
            )),
        columns[0],
    );
    let graph_area = columns[1];
    let detail_area = if wide { columns[2] } else { columns[1] };
    if let Some(node) = ontology.nodes.get(selected) {
        if wide || !inspect {
            let lines = connections(
                node,
                ontology,
                graph_area.width.saturating_sub(2) as usize,
                lang,
                no_color,
            );
            let paragraph = Paragraph::new(lines)
                .style(text_style(PALE, no_color))
                .block(panel(
                    lang.choose("CONEXÕES • PgUp/PgDn", "CONNECTIONS • PgUp/PgDn"),
                    GREEN,
                    no_color,
                ));
            let paragraph = if graph_area.width.saturating_sub(2) < 26 {
                paragraph.wrap(Wrap { trim: false })
            } else {
                paragraph
            };
            max_scroll = max_scroll.max(scrolled(frame, graph_area, paragraph, *scroll));
        }
        if wide || inspect {
            max_scroll = max_scroll.max(scrolled(
                frame,
                detail_area,
                Paragraph::new(inspector(node, ontology, lang))
                    .style(text_style(PALE, no_color))
                    .wrap(Wrap { trim: false })
                    .block(panel(
                        lang.choose("INSPETOR • Enter/F1", "INSPECTOR • Enter/F1"),
                        CYAN,
                        no_color,
                    )),
                *scroll,
            ));
        }
    } else {
        frame.render_widget(
            Paragraph::new(lang.choose("Nenhuma ontologia declarada neste grafo.\n\nTipos e conexões aparecem após declarar o schema.", "No ontology declared for this graph.\n\nTypes and connections appear after declaring a schema.")).style(text_style(PALE, no_color)).wrap(Wrap { trim: false }).block(panel(lang.choose("ONTOLOGIA", "ONTOLOGY"), GREEN, no_color)),
            graph_area,
        );
    }
    *scroll = (*scroll).min(max_scroll);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, style::Color, Terminal};

    fn schema() -> OntologySummary {
        OntologySummary {
            nodes: vec![
                OntologyNode {
                    label: "Customer".into(),
                    properties: vec!["email: String".into()],
                    indexed_fields: vec!["email".into()],
                },
                OntologyNode {
                    label: "Company".into(),
                    ..Default::default()
                },
                OntologyNode {
                    label: "Isolated".into(),
                    ..Default::default()
                },
            ],
            edges: vec![
                OntologyEdge {
                    name: "WORKS_AT".into(),
                    source: "Customer".into(),
                    target: "Company".into(),
                    properties: vec!["since: DateTime".into()],
                },
                OntologyEdge {
                    name: "KNOWS".into(),
                    source: "Customer".into(),
                    target: "Customer".into(),
                    ..Default::default()
                },
                OntologyEdge {
                    name: "EMPLOYS".into(),
                    source: "Company".into(),
                    target: "Customer".into(),
                    ..Default::default()
                },
            ],
        }
    }

    fn draw(
        schema: &OntologySummary,
        width: u16,
        height: u16,
        selected: usize,
        scroll: u16,
        inspect: bool,
        lang: Language,
    ) -> (String, Terminal<TestBackend>) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut scroll = scroll;
        terminal
            .draw(|frame| {
                render(
                    frame,
                    frame.area(),
                    schema,
                    selected,
                    &mut scroll,
                    inspect,
                    lang,
                    true,
                )
            })
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        (text, terminal)
    }

    #[test]
    fn wide_schema_has_directed_boxes_and_inspector() {
        let (text, terminal) = draw(&schema(), 160, 48, 0, 0, false, Language::En);
        for expected in [
            "Customer",
            "Company",
            "WORKS_AT",
            "EMPLOYS",
            "self-loop",
            "PROPERTIES",
            "DECLARED INDEXES",
            "OUTGOING",
            "INCOMING",
            "email: String",
            "since: DateTime",
            "┌",
            "─>",
        ] {
            assert!(text.contains(expected), "missing {expected}");
        }
        assert!(terminal
            .backend()
            .buffer()
            .content
            .iter()
            .all(|cell| cell.fg == Color::Reset && cell.bg == Color::Reset));
    }

    #[test]
    fn compact_toggle_and_localization_preserve_canonical_names() {
        let (graph, _) = draw(&schema(), 80, 24, 0, 0, false, Language::PtBr);
        assert!(graph.contains("CONEXÕES"));
        assert!(graph.contains("WORKS_AT"));
        assert!(!graph.contains("PROPRIEDADES"));
        let (inspector, _) = draw(&schema(), 80, 24, 0, 0, true, Language::PtBr);
        assert!(inspector.contains("PROPRIEDADES"));
        assert!(inspector.contains("ÍNDICES DECLARADOS"));
        assert!(inspector.contains("Customer -[:WORKS_AT]-> Company"));
    }

    #[test]
    fn schema_signatures_remain_distinct_and_scrollable() {
        let mut ontology = schema();
        ontology.edges.extend((0..12).map(|i| OntologyEdge {
            name: format!("DECLARED_{i}"),
            source: "Customer".into(),
            target: "Company".into(),
            ..Default::default()
        }));
        ontology.edges.push(OntologyEdge {
            name: "WORKS_AT".into(),
            source: "Customer".into(),
            target: "UndeclaredEndpoint".into(),
            ..Default::default()
        });
        let detail = inspector(&ontology.nodes[0], &ontology, Language::En);
        assert!(detail.contains("Customer -[:WORKS_AT]-> Company"));
        assert!(detail.contains("Customer -[:WORKS_AT]-> UndeclaredEndpoint"));
        let (scrolled, _) = draw(&ontology, 80, 24, 0, u16::MAX, false, Language::En);
        assert!(scrolled.contains("DECLARED_11"));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut scroll = u16::MAX;
        terminal
            .draw(|frame| {
                render(
                    frame,
                    frame.area(),
                    &ontology,
                    0,
                    &mut scroll,
                    false,
                    Language::En,
                    true,
                )
            })
            .unwrap();
        assert!(scroll > 0 && scroll < u16::MAX);
        let last_page = terminal.backend().buffer().clone();
        scroll -= 1;
        terminal
            .draw(|frame| {
                render(
                    frame,
                    frame.area(),
                    &ontology,
                    0,
                    &mut scroll,
                    false,
                    Language::En,
                    true,
                )
            })
            .unwrap();
        assert_ne!(&last_page, terminal.backend().buffer());
        let (company, _) = draw(&ontology, 80, 24, 1, 0, false, Language::En);
        assert!(!company.contains("self-loop"));
        ontology.edges[0].name = "UNTRUSTED\x1b[31m\nNAME".into();
        let (escaped, _) = draw(&ontology, 160, 48, 0, 0, false, Language::En);
        assert!(escaped.contains("UNTRUSTED\\u{1b}"));
        assert!(!escaped.contains('\x1b'));
    }

    #[test]
    fn empty_isolated_tiny_and_untrusted_names_are_safe() {
        let (empty, _) = draw(
            &OntologySummary::default(),
            80,
            24,
            usize::MAX,
            u16::MAX,
            false,
            Language::En,
        );
        assert!(empty.contains("No ontology declared"));
        let (isolated, _) = draw(&schema(), 80, 24, 2, 0, false, Language::En);
        assert!(isolated.contains("Isolated type"));
        let mut schema = schema();
        schema.nodes[0].label = "Untrusted\x1b[31m\n公司".repeat(20);
        // Keep the long label connected so box truncation is exercised too.
        for edge in &mut schema.edges {
            if edge.source == "Customer" {
                edge.source.clone_from(&schema.nodes[0].label);
            }
            if edge.target == "Customer" {
                edge.target.clone_from(&schema.nodes[0].label);
            }
        }
        schema.nodes[0].properties.push("property\x07\tend".into());
        for (width, height) in [(0, 0), (1, 1), (10, 3), (80, 24), (100, 24), (120, 36)] {
            for inspect in [false, true] {
                draw(
                    &schema,
                    width,
                    height,
                    usize::MAX,
                    u16::MAX,
                    inspect,
                    Language::En,
                );
            }
        }
        let (text, _) = draw(&schema, 160, 48, 0, 0, true, Language::En);
        assert!(!text.contains('\x1b'));
        assert!(!text.contains('\x07'));
        assert!(text.contains("\\u{1b}"));
    }
}
