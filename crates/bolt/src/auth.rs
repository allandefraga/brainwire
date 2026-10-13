//! Service-token authentication policy for protected Bolt sessions.

use std::{collections::BTreeMap, sync::Arc};

use serde::{
    de::{Error, MapAccess, Visitor},
    Deserialize, Deserializer,
};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::packstream::PackValue;
use brainwire_core::acl::ReadVisibility;

#[derive(Clone)]
pub enum AuthMode {
    /// Development mode; never enable on an untrusted network.
    AllowAll,
    Required(Arc<AuthPolicy>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GraphAccess {
    Read,
    Write,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub id: String,
    pub graphs: BTreeMap<String, GraphAccess>,
    pub instance_admin: bool,
    /// Trusted static service policy; absence preserves legacy graph-wide access.
    pub node_visibility: Option<ReadVisibility>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessContext {
    Unrestricted,
    Authenticated(Principal),
    Unauthenticated,
}

#[derive(Debug, Clone, Copy, thiserror::Error)]
pub enum AuthError {
    #[error("invalid authentication policy")]
    InvalidPolicy,
    #[error("authentication failed")]
    AuthenticationFailed,
}

// Verifiers deliberately do not implement Debug or expose their digest.
struct Verifier {
    principal: Principal,
    digest: [u8; 32],
}
pub struct AuthPolicy {
    principals: Vec<Verifier>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyFile {
    principals: Vec<PolicyPrincipal>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyPrincipal {
    id: String,
    token_sha256: String,
    #[serde(deserialize_with = "unique_grants")]
    graphs: BTreeMap<String, GraphAccess>,
    instance_admin: bool,
    #[serde(default, deserialize_with = "non_null_visibility")]
    node_visibility: Option<ReadVisibility>,
}

fn non_null_visibility<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<ReadVisibility>, D::Error> {
    ReadVisibility::deserialize(deserializer).map(Some)
}

fn unique_grants<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, GraphAccess>, D::Error> {
    struct Grants;
    impl<'de> Visitor<'de> for Grants {
        type Value = BTreeMap<String, GraphAccess>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("unique graph grants")
        }
        fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
            let mut grants = BTreeMap::new();
            while let Some((graph, access)) = map.next_entry::<String, GraphAccess>()? {
                if grants.insert(graph, access).is_some() {
                    return Err(M::Error::custom("duplicate graph grant"));
                }
            }
            Ok(grants)
        }
    }
    deserializer.deserialize_map(Grants)
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
fn decode_hash(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut digest = [0; 32];
    for (out, pair) in digest.iter_mut().zip(value.as_bytes().as_chunks::<2>().0) {
        let digit = |b: u8| {
            if b.is_ascii_digit() {
                b - b'0'
            } else {
                b.to_ascii_lowercase() - b'a' + 10
            }
        };
        *out = digit(pair[0]) * 16 + digit(pair[1]);
    }
    Some(digest)
}

impl AuthPolicy {
    pub fn from_json(bytes: &[u8]) -> Result<Self, AuthError> {
        if bytes.len() > 1024 * 1024 {
            return Err(AuthError::InvalidPolicy);
        }
        let file: PolicyFile =
            serde_json::from_slice(bytes).map_err(|_| AuthError::InvalidPolicy)?;
        if file.principals.is_empty() || file.principals.len() > 64 {
            return Err(AuthError::InvalidPolicy);
        }
        let mut principals = Vec::<Verifier>::new();
        for entry in file.principals {
            if !valid_name(&entry.id)
                || entry.graphs.is_empty()
                || entry.graphs.keys().any(|g| !valid_name(g))
                || principals.iter().any(|v| v.principal.id == entry.id)
                || entry
                    .node_visibility
                    .as_ref()
                    .is_some_and(|policy| policy.validate().is_err())
            {
                return Err(AuthError::InvalidPolicy);
            }
            let digest = decode_hash(&entry.token_sha256).ok_or(AuthError::InvalidPolicy)?;
            principals.push(Verifier {
                principal: Principal {
                    id: entry.id,
                    graphs: entry.graphs,
                    instance_admin: entry.instance_admin,
                    node_visibility: entry.node_visibility,
                },
                digest,
            });
        }
        Ok(Self { principals })
    }

    pub fn authenticate(&self, auth: &PackValue) -> Result<Principal, AuthError> {
        let PackValue::Map(fields) = auth else {
            return Err(AuthError::AuthenticationFailed);
        };
        if fields.len() != 3 {
            return Err(AuthError::AuthenticationFailed);
        }
        let (mut scheme, mut principal, mut credentials) = (None, None, None);
        for (key, value) in fields {
            let PackValue::String(value) = value else {
                return Err(AuthError::AuthenticationFailed);
            };
            let slot = match key.as_str() {
                "scheme" => &mut scheme,
                "principal" => &mut principal,
                "credentials" => &mut credentials,
                _ => return Err(AuthError::AuthenticationFailed),
            };
            if slot.replace(value.as_str()).is_some() {
                return Err(AuthError::AuthenticationFailed);
            }
        }
        let (Some("basic"), Some(id), Some(token)) = (scheme, principal, credentials) else {
            return Err(AuthError::AuthenticationFailed);
        };
        if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(AuthError::AuthenticationFailed);
        }
        let digest: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        let verifier = self.principals.iter().find(|v| v.principal.id == id);
        // Unknown identities still perform the digest comparison; no verifier is returned.
        let expected = verifier.map(|v| &v.digest).unwrap_or(&[0; 32]);
        let matches = bool::from(digest.ct_eq(expected));
        match verifier {
            Some(v) if matches => Ok(v.principal.clone()),
            _ => Err(AuthError::AuthenticationFailed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{message::Request, packstream::PackValue};

    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    // SHA-256 of the exact ASCII token above, not its decoded hex bytes.
    const HASH: &str = "a8ae6e6ee929abea3afcfc5258c8ccd6f85273e0d4626d26c7279f3250f77c8e";

    fn policy_json() -> String {
        format!(
            r#"{{"principals":[{{"id":"reader","token_sha256":"{HASH}","graphs":{{"tickets":"read"}},"instance_admin":false}}]}}"#
        )
    }
    fn auth(token: &str) -> PackValue {
        PackValue::Map(vec![
            ("scheme".into(), PackValue::String("basic".into())),
            ("principal".into(), PackValue::String("reader".into())),
            ("credentials".into(), PackValue::String(token.into())),
        ])
    }

    #[test]
    fn authenticates_exact_ascii_token_and_returns_only_access_identity() {
        let policy = AuthPolicy::from_json(policy_json().as_bytes()).unwrap();
        let principal = policy.authenticate(&auth(TOKEN)).unwrap();
        assert_eq!(principal.id, "reader");
        assert_eq!(principal.graphs.get("tickets"), Some(&GraphAccess::Read));
        assert!(!principal.instance_admin);
        assert!(principal.node_visibility.is_none());
        assert!(!format!("{principal:?}").contains(TOKEN));
        assert!(!format!("{principal:?}").contains(HASH));
        assert!(policy.authenticate(&auth(&TOKEN.to_uppercase())).is_err());
    }

    #[test]
    fn static_node_visibility_is_trusted_validated_and_cannot_be_null() {
        let mut value: serde_json::Value = serde_json::from_str(&policy_json()).unwrap();
        value["principals"][0]["node_visibility"] =
            serde_json::json!({"principals":["user:tenant:alice"]});
        let policy = AuthPolicy::from_json(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(
            policy
                .authenticate(&auth(TOKEN))
                .unwrap()
                .node_visibility
                .unwrap()
                .principals,
            vec!["user:tenant:alice"]
        );
        value["principals"][0]["node_visibility"] = serde_json::json!({});
        let policy = AuthPolicy::from_json(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(policy
            .authenticate(&auth(TOKEN))
            .unwrap()
            .node_visibility
            .unwrap()
            .principals
            .is_empty());
        for invalid in [
            serde_json::Value::Null,
            serde_json::json!({"unknown":true}),
            serde_json::json!({"principals":[""]}),
            serde_json::json!({"principals":["x".repeat(257)]}),
        ] {
            value["principals"][0]["node_visibility"] = invalid;
            assert!(AuthPolicy::from_json(&serde_json::to_vec(&value).unwrap()).is_err());
        }
        let PackValue::Map(mut fields) = auth(TOKEN) else {
            unreachable!()
        };
        fields.push(("node_visibility".into(), PackValue::Map(vec![])));
        assert!(policy.authenticate(&PackValue::Map(fields)).is_err());
    }

    #[test]
    fn rejects_missing_wrong_duplicate_and_unsupported_auth_fields() {
        let policy = AuthPolicy::from_json(policy_json().as_bytes()).unwrap();
        assert!(policy.authenticate(&PackValue::Map(vec![])).is_err());
        assert!(policy.authenticate(&PackValue::Null).is_err());
        for token in ["wrong", &"a".repeat(64), &"g".repeat(64)] {
            let error = policy.authenticate(&auth(token)).unwrap_err();
            assert_eq!(error.to_string(), "authentication failed");
            assert!(!format!("{error:?}").contains(token));
        }
        let PackValue::Map(fields) = auth(TOKEN) else {
            unreachable!()
        };
        for key in ["realm", "parameters", "scheme", "credentials", "extra"] {
            let mut invalid = fields.clone();
            invalid.push((key.into(), PackValue::String(TOKEN.into())));
            assert!(policy.authenticate(&PackValue::Map(invalid)).is_err());
        }
        for (key, value) in [("scheme", "bearer"), ("principal", "unknown")] {
            let mut invalid = fields.clone();
            invalid.iter_mut().find(|(k, _)| k == key).unwrap().1 = PackValue::String(value.into());
            assert!(policy.authenticate(&PackValue::Map(invalid)).is_err());
        }
    }

    #[test]
    fn rejects_policy_ambiguity_unknown_fields_and_invalid_grants() {
        let good = policy_json();
        let principal = good
            .strip_prefix(r#"{"principals":["#)
            .unwrap()
            .strip_suffix("]}")
            .unwrap();
        let cases = [
            good.replace("\"read\"", "\"admin\""),
            good.replace("\"tickets\"", "\"../tickets\""),
            good.replace("\"tickets\":\"read\"", ""),
            good.replace(
                "\"tickets\":\"read\"",
                "\"tickets\":\"read\",\"tickets\":\"write\"",
            ),
            good.replace(
                "\"instance_admin\":false",
                "\"instance_admin\":false,\"extra\":true",
            ),
            good.replace(HASH, "bad"),
            good.replace("\"reader\"", "\"\""),
            format!(r#"{{"principals":[{principal},{principal}]}}"#),
            "{\"principals\":[]}".into(),
            format!(r#"{{"principals":[{}]}}"#, vec![principal; 65].join(",")),
        ];
        for invalid in cases {
            assert!(AuthPolicy::from_json(invalid.as_bytes()).is_err());
        }
    }

    #[test]
    fn hello_and_logon_debug_never_expose_auth_values() {
        for request in [
            Request::Logon { auth: auth(TOKEN) },
            Request::Hello { extra: auth(TOKEN) },
        ] {
            let debug = format!("{request:?}");
            assert!(!debug.contains(TOKEN));
            assert!(!debug.contains("reader"));
        }
    }
    #[test]
    fn policy_bounds_count_size_and_names() {
        let principal: serde_json::Value =
            serde_json::from_str::<serde_json::Value>(&policy_json()).unwrap()["principals"][0]
                .clone();
        let mut principals = Vec::new();
        for i in 0..65 {
            let mut entry = principal.clone();
            entry["id"] = format!("reader{i}").into();
            principals.push(entry);
        }
        let policy = serde_json::json!({"principals":&principals[..64]});
        assert!(AuthPolicy::from_json(&serde_json::to_vec(&policy).unwrap()).is_ok());
        let policy = serde_json::json!({"principals":principals});
        assert!(AuthPolicy::from_json(&serde_json::to_vec(&policy).unwrap()).is_err());
        assert!(AuthPolicy::from_json(&vec![b' '; 1024 * 1024 + 1]).is_err());
        for name in ["x".repeat(129), "graph.name".into(), "gráph".into()] {
            assert!(
                AuthPolicy::from_json(policy_json().replace("tickets", &name).as_bytes()).is_err()
            );
        }
        assert!(AuthPolicy::from_json(
            policy_json()
                .replace("tickets", &"x".repeat(128))
                .as_bytes()
        )
        .is_ok());
    }
}
