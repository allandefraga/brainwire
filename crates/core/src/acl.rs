//! Private, bounded node ACL metadata and client's read visibility rule.
use serde::{Deserialize, Serialize};

pub const MAX_ACL_BYTES: usize = 64 * 1024;
const MAX_ITEMS: usize = 256;
const MAX_TEXT_BYTES: usize = 256;
const GRANT_PREFIX: &str = "knowledge-doc:";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NodeAcl {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
    pub connector_ids: Vec<String>,
    pub source_table: Option<String>,
    pub sources: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReadVisibility {
    pub allow_all: bool,
    pub deny_all: bool,
    pub principals: Vec<String>,
    pub allowed_connectors: Option<Vec<String>>,
    pub open_connectors: Vec<String>,
    pub entry_strict: bool,
    pub knowledge_grants: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AclError {
    #[error("ACL lists require at most 256 nonempty strings of at most 256 bytes")]
    InvalidList,
    #[error("encoded ACL exceeds 64 KiB")]
    TooLarge,
    #[error("knowledge grants require knowledge-doc: followed by 64 lowercase hex characters")]
    InvalidGrant,
    #[error("ACL encoding failed: {0}")]
    Encoding(String),
}

fn text(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_TEXT_BYTES
}
fn list(values: &[String]) -> Result<(), AclError> {
    if values.len() > MAX_ITEMS || values.iter().any(|value| !text(value)) {
        return Err(AclError::InvalidList);
    }
    Ok(())
}
fn grant(value: &str) -> bool {
    value.strip_prefix(GRANT_PREFIX).is_some_and(|suffix| {
        suffix.len() == 64
            && suffix
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
fn document_source(value: &str) -> bool {
    value.starts_with("doc_props__") || value.starts_with("clean/doc_props__")
}
fn overlap(left: &[String], right: &[String]) -> bool {
    left.iter().any(|item| right.contains(item))
}

impl NodeAcl {
    pub fn validate(&self) -> Result<(), AclError> {
        for values in [&self.allow, &self.deny, &self.connector_ids, &self.sources] {
            list(values)?;
        }
        if self
            .source_table
            .as_deref()
            .is_some_and(|value| !text(value))
        {
            return Err(AclError::InvalidList);
        }
        let bytes = serde_json::to_vec(self).map_err(|e| AclError::Encoding(e.to_string()))?;
        if bytes.len().saturating_add(1) > MAX_ACL_BYTES {
            return Err(AclError::TooLarge);
        }
        Ok(())
    }
}

impl ReadVisibility {
    pub fn validate(&self) -> Result<(), AclError> {
        for values in [&self.principals, &self.open_connectors] {
            list(values)?;
        }
        if let Some(values) = &self.allowed_connectors {
            list(values)?;
        }
        if let Some(values) = &self.knowledge_grants {
            list(values)?;
            if values.iter().any(|value| !grant(value)) {
                return Err(AclError::InvalidGrant);
            }
        }
        Ok(())
    }

    /// Exact ENTRY AND (OPEN OR row ACL), with authoritative document grants.
    /// Callers validate metadata before evaluating; absent metadata is row-denied.
    pub fn is_visible(&self, acl: Option<&NodeAcl>, has_document_id: bool) -> bool {
        if self.deny_all {
            return false;
        }
        let empty = NodeAcl::default();
        let acl = acl.unwrap_or(&empty);
        if let Some(grants) = &self.knowledge_grants {
            if acl
                .allow
                .iter()
                .any(|value| value.starts_with(GRANT_PREFIX))
                || acl.source_table.as_deref().is_some_and(document_source)
                || acl.sources.iter().any(|value| document_source(value))
                || has_document_id
            {
                return grants.iter().all(|value| grant(value)) && overlap(&acl.allow, grants);
            }
        }
        if self.allow_all {
            return true;
        }
        if self.principals.is_empty() {
            return false;
        }
        let row = overlap(&self.principals, &acl.allow) && !overlap(&self.principals, &acl.deny);
        let Some(allowed) = &self.allowed_connectors else {
            return row;
        };
        if acl.connector_ids.is_empty() {
            return !self.entry_strict && row;
        }
        if !overlap(&acl.connector_ids, allowed) {
            return false;
        }
        if !self.open_connectors.is_empty()
            && acl
                .connector_ids
                .iter()
                .all(|c| self.open_connectors.contains(c))
        {
            return true;
        }
        row
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn values(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).into()).collect()
    }
    #[test]
    fn truth_table_preserves_entry_open_deny_and_missing_provenance() {
        let mut acl = NodeAcl {
            allow: values(&["user"]),
            connector_ids: values(&["jira"]),
            ..NodeAcl::default()
        };
        let mut policy = ReadVisibility {
            principals: values(&["user"]),
            ..ReadVisibility::default()
        };
        assert!(policy.is_visible(Some(&acl), false));
        assert!(!policy.is_visible(None, false));
        acl.deny = values(&["user"]);
        assert!(!policy.is_visible(Some(&acl), false));
        policy.allowed_connectors = Some(values(&["jira"]));
        policy.open_connectors = values(&["jira"]);
        assert!(policy.is_visible(Some(&acl), false));
        acl.connector_ids.push("closed".into());
        assert!(!policy.is_visible(Some(&acl), false));
        acl.connector_ids = values(&["JIRA"]);
        assert!(!policy.is_visible(Some(&acl), false));
        acl.connector_ids.clear();
        acl.deny.clear();
        assert!(policy.is_visible(Some(&acl), false));
        policy.entry_strict = true;
        assert!(!policy.is_visible(Some(&acl), false));
        policy.allow_all = true;
        assert!(policy.is_visible(None, false));
        policy.deny_all = true;
        assert!(!policy.is_visible(Some(&acl), false));
    }
    #[test]
    fn document_grants_are_exclusive_even_with_allow_all_open_or_row_grants() {
        let token = format!("knowledge-doc:{}", "a".repeat(64));
        let mut acl = NodeAcl {
            allow: vec!["user".into()],
            ..NodeAcl::default()
        };
        let policy = ReadVisibility {
            allow_all: true,
            knowledge_grants: Some(vec![token.clone()]),
            ..ReadVisibility::default()
        };
        assert!(!policy.is_visible(Some(&acl), true));
        for source in ["doc_props__article", "clean/doc_props__article"] {
            acl.source_table = Some(source.into());
            assert!(!policy.is_visible(Some(&acl), false));
        }
        acl.allow.push(token);
        assert!(policy.is_visible(Some(&acl), true));
        let denied = ReadVisibility {
            deny_all: true,
            ..policy.clone()
        };
        assert!(!denied.is_visible(Some(&acl), true));
        let no_grants = ReadVisibility {
            knowledge_grants: Some(vec![]),
            ..policy
        };
        assert!(!no_grants.is_visible(Some(&acl), true));
    }
    #[test]
    fn private_metadata_and_policy_validation_are_bounded_and_strict() {
        assert!(NodeAcl::default().validate().is_ok());
        assert!(NodeAcl {
            allow: vec!["x".into(); 257],
            ..NodeAcl::default()
        }
        .validate()
        .is_err());
        assert!(NodeAcl {
            allow: vec![String::new()],
            ..NodeAcl::default()
        }
        .validate()
        .is_err());
        assert!(NodeAcl {
            allow: vec!["x".repeat(256); 256],
            ..NodeAcl::default()
        }
        .validate()
        .is_err());
        assert!(serde_json::from_str::<NodeAcl>(r#"{"document_id":"fake"}"#).is_err());
        assert!(serde_json::from_str::<ReadVisibility>(r#"{"fake":true}"#).is_err());
        assert!(ReadVisibility {
            knowledge_grants: Some(vec!["knowledge-doc:bad".into()]),
            ..ReadVisibility::default()
        }
        .validate()
        .is_err());
    }
    #[test]
    fn entry_and_open_cross_product_matches_independent_boolean_rule() {
        for principals in [false, true] {
            for allow in [false, true] {
                for deny in [false, true] {
                    for provenance in 0u8..4 {
                        for admitted in [None, Some(0u8), Some(1), Some(2), Some(3)] {
                            for open in 0u8..4 {
                                for strict in [false, true] {
                                    let connectors = |bits: u8| {
                                        (0..2)
                                            .filter(|i| bits & (1 << i) != 0)
                                            .map(|i| format!("connector-{i}"))
                                            .collect::<Vec<_>>()
                                    };
                                    let acl = NodeAcl {
                                        allow: if allow { vec!["user".into()] } else { vec![] },
                                        deny: if deny { vec!["user".into()] } else { vec![] },
                                        connector_ids: connectors(provenance),
                                        ..NodeAcl::default()
                                    };
                                    let policy = ReadVisibility {
                                        principals: if principals {
                                            vec!["user".into()]
                                        } else {
                                            vec![]
                                        },
                                        allowed_connectors: admitted.map(connectors),
                                        open_connectors: connectors(open),
                                        entry_strict: strict,
                                        ..ReadVisibility::default()
                                    };
                                    let row = principals && allow && !deny;
                                    let expected = principals
                                        && match admitted {
                                            None => row,
                                            Some(_) if provenance == 0 => !strict && row,
                                            Some(bits) => {
                                                (bits & provenance != 0)
                                                    && ((open != 0 && provenance & !open == 0)
                                                        || row)
                                            }
                                        };
                                    assert_eq!(policy.is_visible(Some(&acl), false), expected);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
