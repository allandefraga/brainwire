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
    /// Names given through `CREATE INDEX`, by property. These indexes survive
    /// a later ontology re-apply; ontology-only ones use `index_name`'s default.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub index_names: BTreeMap<String, String>,
    /// Indexed properties still being backfilled online. Writers maintain
    /// their entries, but queries must not use them until they leave this list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub populating: Vec<String>,
    /// Column ids of dropped properties. They are never reused, so values
    /// still stored under them can never resurface under a new property.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retired_columns: Vec<u16>,
}

impl NodeType {
    pub fn column(&self, name: &str) -> Option<u16> {
        self.columns.iter().find(|c| c.name == name).map(|c| c.id)
    }

    pub fn index_name(&self, field: &str) -> String {
        self.index_names
            .get(field)
            .cloned()
            .unwrap_or_else(|| format!("index_{}_{field}", self.label))
    }

    /// Declared and fully built: a query may answer from this index.
    pub fn index_online(&self, field: &str) -> bool {
        self.indexed.iter().any(|f| f == field) && !self.populating.iter().any(|f| f == field)
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
    #[error("more than 4095 node types")]
    TooManyTypes,
    #[error("entity_join must look like 'column -> Label.property': {0}")]
    BadEntityJoin(String),
    #[error("no ids left for {0}")]
    IdSpaceExhausted(String),
}

fn is_reserved(name: &str) -> bool {
    name.to_ascii_lowercase().starts_with("acl_")
}

/// Keeps the previous columns (so old rows decode) and appends new names.
/// `owner` names whose columns these are, for the `IdSpaceExhausted` message
/// (e.g. "Customer", "WORKS_AT", "interaction").
fn merge_columns(
    previous: &[Column],
    retired: &[u16],
    names: &[String],
    owner: &str,
) -> Result<Vec<Column>, SchemaError> {
    let mut cols = previous.to_vec();
    for name in names.iter().filter(|n| !is_reserved(n)) {
        if !cols.iter().any(|c| &c.name == name) {
            let id = match cols
                .iter()
                .map(|c| c.id)
                .chain(retired.iter().copied())
                .max()
            {
                Some(max_id) => max_id
                    .checked_add(1)
                    .ok_or_else(|| SchemaError::IdSpaceExhausted(format!("columns of {owner}")))?,
                None => 0,
            };
            cols.push(Column {
                id,
                name: name.clone(),
            });
        }
    }
    Ok(cols)
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
        let mut labels_seen: Vec<String> = Vec::new();
        for spec in &mapping.nodes {
            if !labels_seen.contains(&spec.label) {
                labels_seen.push(spec.label.clone());
            }
        }
        for label in &labels_seen {
            // Multiple NodeSpecs may share a label: the same conceptual entity
            // sourced from different connectors (e.g. Person <- hubspot_contacts +
            // zendesk_users + jira_users). They merge into one NodeType, mirroring
            // the platform's GraphMappingConfig (memory-worker/graph_mapping/
            // models.py: duplicate_node_labels_must_share_id_field,
            // presentation_is_resolved_per_label).
            let group: Vec<&NodeSpec> =
                mapping.nodes.iter().filter(|s| s.label == *label).collect();

            let mut indexed: Vec<String> = Vec::new();
            for spec in &group {
                for f in spec.er_fields.iter().chain(&spec.indexed_fields) {
                    if !indexed.contains(f) && !is_reserved(f) {
                        indexed.push(f.clone());
                    }
                }
            }
            let mut identifiers: Vec<(String, String)> = Vec::new();
            for spec in &group {
                for (c, t) in &spec.identifier_columns {
                    if !identifiers.iter().any(|(col, _)| col == c) {
                        identifiers.push((c.clone(), t.clone()));
                    }
                }
            }
            // First entry in declaration order that sets a color wins; a later
            // source with no opinion never erases an earlier source's color.
            let display_color = group.iter().find_map(|spec| spec.display_color.clone());
            let properties: Vec<String> = group
                .iter()
                .flat_map(|spec| spec.properties.clone())
                .collect();

            match node_types.iter_mut().find(|t| t.label == *label) {
                Some(t) => {
                    t.columns = merge_columns(&t.columns, &t.retired_columns, &properties, label)?;
                    // Indexes created online are not in the ontology file;
                    // re-applying it must not silently drop them.
                    for field in t.index_names.keys() {
                        if !indexed.contains(field) {
                            indexed.push(field.clone());
                        }
                    }
                    t.populating.retain(|field| indexed.contains(field));
                    t.indexed = indexed;
                    t.identifiers = identifiers;
                    t.display_color = display_color;
                }
                None => {
                    let next = node_types.iter().map(|t| t.id.0 + 1).max().unwrap_or(1);
                    if next > TypeId::MAX {
                        return Err(SchemaError::TooManyTypes);
                    }
                    node_types.push(NodeType {
                        id: TypeId(next),
                        label: label.clone(),
                        columns: merge_columns(&[], &[], &properties, label)?,
                        indexed,
                        identifiers,
                        display_color,
                        index_names: BTreeMap::new(),
                        populating: vec![],
                        retired_columns: vec![],
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
                    t.columns = merge_columns(&t.columns, &[], &spec.properties, &spec.name)?;
                }
                None => {
                    let next = match edge_types.iter().map(|t| t.id.0).max() {
                        Some(max_id) => max_id
                            .checked_add(1)
                            .ok_or_else(|| SchemaError::IdSpaceExhausted("edge types".into()))?,
                        None => 1,
                    };
                    edge_types.push(EdgeType {
                        id: EdgeTypeId(next),
                        name: spec.name.clone(),
                        signatures: vec![sig],
                        columns: merge_columns(&[], &[], &spec.properties, &spec.name)?,
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
                Some(s) => {
                    s.columns = merge_columns(&s.columns, &[], &spec.properties, &spec.event_type)?
                }
                None => {
                    let next = match streams.iter().map(|s| s.id).max() {
                        Some(max_id) => max_id
                            .checked_add(1)
                            .ok_or_else(|| SchemaError::IdSpaceExhausted("streams".into()))?,
                        None => 1,
                    };
                    streams.push(Stream {
                        id: next,
                        event_type: spec.event_type.clone(),
                        owner,
                        timestamp_field: spec.timestamp_field.clone(),
                        columns: merge_columns(&[], &[], &spec.properties, &spec.event_type)?,
                    });
                }
            }
        }

        let version = prev
            .version
            .checked_add(1)
            .ok_or_else(|| SchemaError::IdSpaceExhausted("catalog version".into()))?;
        Ok(Catalog {
            version,
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
    // TooManyTypes, and don't pin the "malformed/hostile JSON never panics"
    // contract. See task-5-report.md for the rationale.

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

    // --- Fix round 1 (controller ruling): id/version arithmetic must be
    // checked, not wrapping, so a huge ontology returns IdSpaceExhausted
    // instead of silently reusing an id. These build the previous Catalog
    // directly with a column already at u16::MAX, rather than generating
    // 65,536 properties.

    #[test]
    fn exhausting_column_ids_is_an_error() {
        let previous = Catalog {
            version: 1,
            node_types: vec![NodeType {
                id: TypeId(1),
                label: "Customer".into(),
                columns: vec![Column {
                    id: u16::MAX,
                    name: "old".into(),
                }],
                indexed: vec![],
                identifiers: vec![],
                display_color: None,
                index_names: BTreeMap::new(),
                populating: vec![],
                retired_columns: vec![],
            }],
            edge_types: vec![],
            streams: vec![],
        };
        let mapping = GraphMapping {
            version: "2".into(),
            nodes: vec![NodeSpec {
                label: "Customer".into(),
                properties: vec!["old".into(), "new_prop".into()],
                er_fields: vec![],
                indexed_fields: vec![],
                identifier_columns: BTreeMap::new(),
                display_color: None,
            }],
            edges: vec![],
            timeline: vec![],
        };
        assert_eq!(
            Catalog::apply(&mapping, Some(&previous)),
            Err(SchemaError::IdSpaceExhausted("columns of Customer".into()))
        );
    }

    #[test]
    fn exhausting_edge_type_ids_is_an_error() {
        let previous = Catalog {
            version: 1,
            node_types: vec![
                NodeType {
                    id: TypeId(1),
                    label: "Customer".into(),
                    columns: vec![],
                    indexed: vec![],
                    identifiers: vec![],
                    display_color: None,
                    index_names: BTreeMap::new(),
                    populating: vec![],
                    retired_columns: vec![],
                },
                NodeType {
                    id: TypeId(2),
                    label: "Company".into(),
                    columns: vec![],
                    indexed: vec![],
                    identifiers: vec![],
                    display_color: None,
                    index_names: BTreeMap::new(),
                    populating: vec![],
                    retired_columns: vec![],
                },
            ],
            edge_types: vec![EdgeType {
                id: EdgeTypeId(u16::MAX),
                name: "OLD_EDGE".into(),
                signatures: vec![(TypeId(1), TypeId(2))],
                columns: vec![],
            }],
            streams: vec![],
        };
        let mapping = GraphMapping {
            version: "2".into(),
            nodes: vec![
                NodeSpec {
                    label: "Customer".into(),
                    properties: vec![],
                    er_fields: vec![],
                    indexed_fields: vec![],
                    identifier_columns: BTreeMap::new(),
                    display_color: None,
                },
                NodeSpec {
                    label: "Company".into(),
                    properties: vec![],
                    er_fields: vec![],
                    indexed_fields: vec![],
                    identifier_columns: BTreeMap::new(),
                    display_color: None,
                },
            ],
            edges: vec![EdgeSpec {
                name: "NEW_EDGE".into(),
                from: EndpointSpec {
                    label: "Customer".into(),
                },
                to: EndpointSpec {
                    label: "Company".into(),
                },
                properties: vec![],
            }],
            timeline: vec![],
        };
        assert_eq!(
            Catalog::apply(&mapping, Some(&previous)),
            Err(SchemaError::IdSpaceExhausted("edge types".into()))
        );
    }

    // --- Fix round 2 (controller ruling): entries that share a label merge
    // into one NodeType instead of erroring, mirroring the platform's
    // GraphMappingConfig (memory-worker/graph_mapping/models.py:703-835),
    // which explicitly supports one label sourced from several connectors.

    fn node(label: &str, properties: &[&str], display_color: Option<&str>) -> NodeSpec {
        NodeSpec {
            label: label.into(),
            properties: properties.iter().map(|s| s.to_string()).collect(),
            er_fields: vec![],
            indexed_fields: vec![],
            identifier_columns: BTreeMap::new(),
            display_color: display_color.map(String::from),
        }
    }

    #[test]
    fn sharing_a_label_merges_into_one_type() {
        let mapping = GraphMapping {
            version: "1".into(),
            nodes: vec![
                node("Customer", &["name", "email"], None),
                node("Customer", &["email", "phone", "acl_allow"], None),
            ],
            edges: vec![],
            timeline: vec![],
        };
        let cat = Catalog::apply(&mapping, None).unwrap();
        assert_eq!(
            cat.node_types
                .iter()
                .filter(|t| t.label == "Customer")
                .count(),
            1
        );
        let customer = cat.node_type("Customer").unwrap();
        assert_eq!(customer.column("name"), Some(0));
        assert_eq!(customer.column("email"), Some(1));
        assert_eq!(customer.column("phone"), Some(2));
        assert!(customer.column("acl_allow").is_none());
    }

    #[test]
    fn reapplying_shared_label_keeps_ids_stable_and_appends() {
        fn two_entries(second_properties: &[&str]) -> GraphMapping {
            GraphMapping {
                version: "1".into(),
                nodes: vec![
                    node("Customer", &["name", "email"], None),
                    node("Customer", second_properties, None),
                ],
                edges: vec![],
                timeline: vec![],
            }
        }

        let first = Catalog::apply(&two_entries(&["email", "phone", "acl_allow"]), None).unwrap();
        let second = Catalog::apply(
            &two_entries(&["email", "phone", "acl_allow", "cpf"]),
            Some(&first),
        )
        .unwrap();
        let customer = second.node_type("Customer").unwrap();
        assert_eq!(customer.column("name"), Some(0));
        assert_eq!(customer.column("email"), Some(1));
        assert_eq!(customer.column("phone"), Some(2));
        assert_eq!(customer.column("cpf"), Some(3));
    }

    #[test]
    fn shared_label_display_color_is_the_first_one_set() {
        let mapping = GraphMapping {
            version: "1".into(),
            nodes: vec![
                node("Customer", &[], None),
                node("Customer", &[], Some("#111111")),
                node("Customer", &[], Some("#222222")),
            ],
            edges: vec![],
            timeline: vec![],
        };
        let cat = Catalog::apply(&mapping, None).unwrap();
        assert_eq!(
            cat.node_type("Customer").unwrap().display_color,
            Some("#111111".into())
        );
    }

    #[test]
    fn edge_from_a_shared_label_resolves_to_the_merged_type() {
        let mapping = GraphMapping {
            version: "1".into(),
            nodes: vec![
                node("Customer", &["name"], None),
                node("Customer", &["phone"], None),
                node("Company", &["name"], None),
            ],
            edges: vec![EdgeSpec {
                name: "WORKS_AT".into(),
                from: EndpointSpec {
                    label: "Customer".into(),
                },
                to: EndpointSpec {
                    label: "Company".into(),
                },
                properties: vec![],
            }],
            timeline: vec![],
        };
        let cat = Catalog::apply(&mapping, None).unwrap();
        let customer_id = cat.node_type("Customer").unwrap().id;
        let company_id = cat.node_type("Company").unwrap().id;
        assert_eq!(
            cat.edge_type("WORKS_AT").unwrap().signatures,
            vec![(customer_id, company_id)]
        );
    }

    #[test]
    fn reapplying_the_ontology_keeps_online_indexes_and_their_state() {
        let mut first = Catalog::apply(&fixture(), None).unwrap();
        let customer = first
            .node_types
            .iter_mut()
            .find(|t| t.label == "Customer")
            .unwrap();
        customer.indexed.push("name".into());
        customer.populating.push("name".into());
        customer
            .index_names
            .insert("name".into(), "customer_name".into());
        let second = Catalog::apply(&fixture(), Some(&first)).unwrap();
        let customer = second.node_type("Customer").unwrap();
        assert_eq!(customer.indexed, vec!["email", "phone", "segment", "name"]);
        assert!(!customer.index_online("name"));
        assert!(customer.index_online("email"));
        assert_eq!(customer.index_name("name"), "customer_name");
        assert_eq!(customer.index_name("email"), "index_Customer_email");
    }

    #[test]
    fn retired_column_ids_are_never_reused() {
        let mut first = Catalog::apply(&fixture(), None).unwrap();
        let customer = first
            .node_types
            .iter_mut()
            .find(|t| t.label == "Customer")
            .unwrap();
        let last = customer.columns.pop().unwrap();
        customer.retired_columns.push(last.id);
        let second = Catalog::apply(&fixture(), Some(&first)).unwrap();
        let readded = second.node_type("Customer").unwrap().column(&last.name);
        assert_eq!(readded, Some(last.id + 1));
    }
}
