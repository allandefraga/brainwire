//! The ontology becomes the schema (spec 2.1). Input: the platform's
//! GraphMappingConfig as JSON. Output: a versioned catalog with stable ids.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ids::{EdgeTypeId, TypeId};

#[derive(Debug, Clone, Deserialize)]
pub struct GraphMapping {
    pub version: String,
    pub nodes: Vec<NodeSpec>,
    #[serde(default)]
    pub edges: Vec<EdgeSpec>,
    #[serde(default)]
    pub timeline: Vec<TimelineSpec>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NodeSpec {
    pub label: String,
    #[serde(default)]
    pub properties: Vec<String>,
    #[serde(default)]
    pub er_fields: Vec<String>,
    #[serde(default)]
    pub indexed_fields: Vec<String>,
    #[serde(default)]
    pub identifier_columns: BTreeMap<String, String>,
    #[serde(default)]
    pub display_color: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EdgeSpec {
    #[serde(rename = "type")]
    pub name: String,
    pub from: EndpointSpec,
    pub to: EndpointSpec,
    #[serde(default)]
    pub properties: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EndpointSpec {
    pub label: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TimelineSpec {
    pub event_type: String,
    pub entity_join: String,
    pub timestamp_field: String,
    #[serde(default)]
    pub properties: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Column {
    pub id: u16,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeType {
    pub id: TypeId,
    pub label: String,
    pub columns: Vec<Column>,
    pub indexed: Vec<String>,
    pub identifiers: Vec<(String, String)>,
    pub display_color: Option<String>,
}

impl NodeType {
    pub fn column(&self, name: &str) -> Option<u16> {
        self.columns.iter().find(|c| c.name == name).map(|c| c.id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EdgeType {
    pub id: EdgeTypeId,
    pub name: String,
    pub signatures: Vec<(TypeId, TypeId)>,
    pub columns: Vec<Column>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stream {
    pub id: u16,
    pub event_type: String,
    pub owner: TypeId,
    pub timestamp_field: String,
    pub columns: Vec<Column>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Catalog {
    pub version: u32,
    pub node_types: Vec<NodeType>,
    pub edge_types: Vec<EdgeType>,
    pub streams: Vec<Stream>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SchemaError {
    #[error("unknown label {0}")]
    UnknownLabel(String),
    #[error("label {0} is declared twice")]
    DuplicateLabel(String),
    #[error("more than 4095 node types")]
    TooManyTypes,
    #[error("entity_join must look like 'column -> Label.property': {0}")]
    BadEntityJoin(String),
}

fn is_reserved(name: &str) -> bool {
    name.to_ascii_lowercase().starts_with("acl_")
}

/// Keeps the previous columns (so old rows decode) and appends new names.
fn merge_columns(previous: &[Column], names: &[String]) -> Vec<Column> {
    let mut cols = previous.to_vec();
    for name in names.iter().filter(|n| !is_reserved(n)) {
        if !cols.iter().any(|c| &c.name == name) {
            let id = cols.iter().map(|c| c.id + 1).max().unwrap_or(0);
            cols.push(Column {
                id,
                name: name.clone(),
            });
        }
    }
    cols
}

fn owner_label(entity_join: &str) -> Result<String, SchemaError> {
    let bad = || SchemaError::BadEntityJoin(entity_join.to_string());
    let (_, target) = entity_join.split_once("->").ok_or_else(bad)?;
    let (label, _) = target.trim().split_once('.').ok_or_else(bad)?;
    if label.is_empty() {
        return Err(bad());
    }
    Ok(label.to_string())
}

impl Catalog {
    pub fn node_type(&self, label: &str) -> Option<&NodeType> {
        self.node_types.iter().find(|t| t.label == label)
    }

    pub fn edge_type(&self, name: &str) -> Option<&EdgeType> {
        self.edge_types.iter().find(|t| t.name == name)
    }

    pub fn apply(
        mapping: &GraphMapping,
        previous: Option<&Catalog>,
    ) -> Result<Catalog, SchemaError> {
        let empty = Catalog {
            version: 0,
            node_types: vec![],
            edge_types: vec![],
            streams: vec![],
        };
        let prev = previous.unwrap_or(&empty);

        let mut node_types = prev.node_types.clone();
        let mut seen = Vec::new();
        for spec in &mapping.nodes {
            if seen.contains(&spec.label) {
                return Err(SchemaError::DuplicateLabel(spec.label.clone()));
            }
            seen.push(spec.label.clone());
            let mut indexed: Vec<String> = Vec::new();
            for f in spec.er_fields.iter().chain(&spec.indexed_fields) {
                if !indexed.contains(f) && !is_reserved(f) {
                    indexed.push(f.clone());
                }
            }
            let identifiers: Vec<(String, String)> = spec
                .identifier_columns
                .iter()
                .map(|(c, t)| (c.clone(), t.clone()))
                .collect();
            match node_types.iter_mut().find(|t| t.label == spec.label) {
                Some(t) => {
                    t.columns = merge_columns(&t.columns, &spec.properties);
                    t.indexed = indexed;
                    t.identifiers = identifiers;
                    t.display_color = spec.display_color.clone();
                }
                None => {
                    let next = node_types.iter().map(|t| t.id.0 + 1).max().unwrap_or(1);
                    if next > TypeId::MAX {
                        return Err(SchemaError::TooManyTypes);
                    }
                    node_types.push(NodeType {
                        id: TypeId(next),
                        label: spec.label.clone(),
                        columns: merge_columns(&[], &spec.properties),
                        indexed,
                        identifiers,
                        display_color: spec.display_color.clone(),
                    });
                }
            }
        }
        let type_of = |label: &str| {
            node_types
                .iter()
                .find(|t| t.label == label)
                .map(|t| t.id)
                .ok_or_else(|| SchemaError::UnknownLabel(label.to_string()))
        };

        let mut edge_types = prev.edge_types.clone();
        for spec in &mapping.edges {
            let sig = (type_of(&spec.from.label)?, type_of(&spec.to.label)?);
            match edge_types.iter_mut().find(|t| t.name == spec.name) {
                Some(t) => {
                    if !t.signatures.contains(&sig) {
                        t.signatures.push(sig);
                    }
                    t.columns = merge_columns(&t.columns, &spec.properties);
                }
                None => {
                    let next = edge_types.iter().map(|t| t.id.0 + 1).max().unwrap_or(1);
                    edge_types.push(EdgeType {
                        id: EdgeTypeId(next),
                        name: spec.name.clone(),
                        signatures: vec![sig],
                        columns: merge_columns(&[], &spec.properties),
                    });
                }
            }
        }

        let mut streams = prev.streams.clone();
        for spec in &mapping.timeline {
            let owner = type_of(&owner_label(&spec.entity_join)?)?;
            match streams
                .iter_mut()
                .find(|s| s.event_type == spec.event_type && s.owner == owner)
            {
                Some(s) => s.columns = merge_columns(&s.columns, &spec.properties),
                None => {
                    let next = streams.iter().map(|s| s.id + 1).max().unwrap_or(1);
                    streams.push(Stream {
                        id: next,
                        event_type: spec.event_type.clone(),
                        owner,
                        timestamp_field: spec.timestamp_field.clone(),
                        columns: merge_columns(&[], &spec.properties),
                    });
                }
            }
        }

        Ok(Catalog {
            version: prev.version + 1,
            node_types,
            edge_types,
            streams,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> GraphMapping {
        serde_json::from_str(include_str!("../tests/fixtures/graph_mapping.json")).unwrap()
    }

    #[test]
    fn builds_the_catalog() {
        let cat = Catalog::apply(&fixture(), None).unwrap();
        assert_eq!(cat.version, 1);
        let customer = cat.node_type("Customer").unwrap();
        assert_eq!(customer.id, TypeId(1));
        assert_eq!(cat.node_type("Company").unwrap().id, TypeId(2));
        assert!(customer.column("acl_allow").is_none());
        assert_eq!(customer.indexed, vec!["email", "phone", "segment"]);
        assert_eq!(customer.identifiers.len(), 2);
        let in_segment = cat.edge_type("IN_SEGMENT").unwrap();
        assert_eq!(
            in_segment.signatures,
            vec![(TypeId(2), TypeId(3)), (TypeId(1), TypeId(3))]
        );
        assert_eq!(cat.streams[0].owner, TypeId(1));
    }

    #[test]
    fn ids_are_stable_across_applications() {
        let first = Catalog::apply(&fixture(), None).unwrap();
        let mut next = fixture();
        next.nodes.insert(
            0,
            serde_json::from_str(r#"{"label": "Product", "properties": ["sku"]}"#).unwrap(),
        );
        next.nodes[1].properties.insert(0, "nickname".into());
        let second = Catalog::apply(&next, Some(&first)).unwrap();
        assert_eq!(second.version, 2);
        assert_eq!(second.node_type("Customer").unwrap().id, TypeId(1));
        assert_eq!(second.node_type("Product").unwrap().id, TypeId(4));
        let customer = second.node_type("Customer").unwrap();
        assert_eq!(
            customer.column("name"),
            first.node_type("Customer").unwrap().column("name")
        );
        assert!(customer.column("nickname").is_some());
    }

    #[test]
    fn unknown_edge_label_is_an_error() {
        let mut m = fixture();
        m.edges[0].to.label = "Nope".into();
        assert_eq!(
            Catalog::apply(&m, None),
            Err(SchemaError::UnknownLabel("Nope".into()))
        );
    }

    #[test]
    fn bad_entity_join_is_an_error() {
        let mut m = fixture();
        m.timeline[0].entity_join = "customer_email".into();
        assert!(matches!(
            Catalog::apply(&m, None),
            Err(SchemaError::BadEntityJoin(_))
        ));
    }

    #[test]
    fn catalog_round_trips_as_json() {
        let cat = Catalog::apply(&fixture(), None).unwrap();
        let back: Catalog = serde_json::from_str(&serde_json::to_string(&cat).unwrap()).unwrap();
        assert_eq!(back, cat);
    }

    // --- Added beyond the brief: the ontology is external input, and the
    // task rules require every SchemaError variant to be exercised. The
    // brief's own tests cover UnknownLabel and BadEntityJoin but not
    // DuplicateLabel or TooManyTypes, and don't pin the "malformed/hostile
    // JSON never panics" contract. See task-5-report.md for the rationale.

    #[test]
    fn duplicate_node_label_is_an_error() {
        let mut m = fixture();
        m.nodes.push(m.nodes[0].clone());
        assert_eq!(
            Catalog::apply(&m, None),
            Err(SchemaError::DuplicateLabel("Customer".into()))
        );
    }

    #[test]
    fn too_many_node_types_is_an_error() {
        let limit = u32::from(TypeId::MAX) + 1;
        let nodes = (0..limit)
            .map(|i| NodeSpec {
                label: format!("Type{i}"),
                properties: vec![],
                er_fields: vec![],
                indexed_fields: vec![],
                identifier_columns: BTreeMap::new(),
                display_color: None,
            })
            .collect();
        let m = GraphMapping {
            version: "1".into(),
            nodes,
            edges: vec![],
            timeline: vec![],
        };
        assert_eq!(Catalog::apply(&m, None), Err(SchemaError::TooManyTypes));
    }

    #[test]
    fn malformed_json_is_an_error_not_a_panic() {
        let syntactically_broken: Result<GraphMapping, _> = serde_json::from_str("{not valid json");
        assert!(syntactically_broken.is_err());

        let wrong_shape: Result<GraphMapping, _> =
            serde_json::from_str(r#"{"version": "1", "nodes": "not-an-array"}"#);
        assert!(wrong_shape.is_err());
    }

    #[test]
    fn missing_previous_catalog_starts_fresh_and_node_type_lookup_misses_are_none() {
        let cat = Catalog::apply(&fixture(), None).unwrap();
        assert!(cat.node_type("DoesNotExist").is_none());
        assert!(cat.edge_type("DOES_NOT_EXIST").is_none());
    }
}
