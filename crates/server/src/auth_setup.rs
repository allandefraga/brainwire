//! Offline service credential initialization; no default password or secret output.

use std::path::Path;

pub(crate) fn run(directory: &Path, graph: &str) -> Result<(), String> {
    initialize(directory, graph)?;
    println!("Created policy.json, admin.json and health.json; no credentials printed.");
    println!(
        "Set BRAINWIRE_SERVER_AUTH_FILE to policy.json and restart the server to load grants."
    );
    Ok(())
}

#[cfg(unix)]
fn initialize(directory: &Path, graph: &str) -> Result<(), String> {
    use serde_json::json;
    use sha2::{Digest, Sha256};
    use std::{
        fs::{DirBuilder, File, OpenOptions},
        io::{Read, Write},
        os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    };
    if graph.is_empty()
        || graph.len() > 128
        || !graph
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err("invalid graph name".into());
    }
    let mut clients = Vec::new();
    let mut principals = Vec::new();
    let mut random = File::open("/dev/urandom").map_err(|_| "cannot open OS random source")?;
    for (name, access, instance_admin) in [("admin", "write", true), ("health", "read", false)] {
        let mut bytes = [0u8; 32];
        random
            .read_exact(&mut bytes)
            .map_err(|_| "cannot read OS random source")?;
        let token = bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let verifier = format!("{:x}", Sha256::digest(token.as_bytes()));
        clients.push((name, json!({"principal": name, "credentials": token})));
        principals.push(json!({"id": name, "token_sha256": verifier, "graphs": {graph: access}, "instance_admin": instance_admin}));
    }
    let policy = json!({"principals": principals});
    let encoded = serde_json::to_vec(&policy).map_err(|_| "cannot encode authentication policy")?;
    brainwire_bolt::auth::AuthPolicy::from_json(&encoded)
        .map_err(|_| "invalid generated authentication policy")?;
    DirBuilder::new()
        .mode(0o700)
        .create(directory)
        .map_err(|_| "cannot create new private credentials directory; choose a fresh path")?;
    for (name, value) in clients
        .into_iter()
        .chain(std::iter::once(("policy", policy)))
    {
        let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600)
            .open(directory.join(format!("{name}.json")))
            .map_err(|_| "cannot create private credentials file; directory may be incomplete, use a fresh path")?;
        serde_json::to_writer_pretty(&mut file, &value)
            .map_err(|_| "cannot write credentials file; use a fresh directory")?;
        file.write_all(b"\n")
            .and_then(|()| file.sync_all())
            .map_err(|_| "cannot persist credentials file; use a fresh directory")?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn initialize(_directory: &Path, _graph: &str) -> Result<(), String> {
    Err("auth init requires Unix private-file permissions; run it in the Linux container".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use brainwire_bolt::{auth::AuthPolicy, packstream::PackValue};
    use serde_json::Value;

    #[test]
    #[cfg(unix)]
    fn credentials_authenticate_with_scoped_admin_and_health_grants() {
        use std::os::unix::fs::PermissionsExt;
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("auth");
        initialize(&directory, "tickets").unwrap();
        assert_eq!(
            std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let policy_bytes = std::fs::read(directory.join("policy.json")).unwrap();
        let policy = AuthPolicy::from_json(&policy_bytes).unwrap();
        let mut tokens = Vec::new();
        for name in ["admin", "health"] {
            let path = directory.join(format!("{name}.json"));
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            let credentials: Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            let token = credentials["credentials"].as_str().unwrap();
            assert_eq!(token.len(), 64);
            assert!(token.bytes().all(|byte| byte.is_ascii_hexdigit()));
            assert!(!String::from_utf8_lossy(&policy_bytes).contains(token));
            let auth = PackValue::Map(vec![
                ("scheme".into(), PackValue::String("basic".into())),
                ("principal".into(), PackValue::String(name.into())),
                ("credentials".into(), PackValue::String(token.into())),
            ]);
            let principal = policy.authenticate(&auth).unwrap();
            assert_eq!(principal.instance_admin, name == "admin");
            assert_eq!(principal.graphs.len(), 1);
            assert_eq!(
                principal.graphs["tickets"],
                if name == "admin" {
                    brainwire_bolt::auth::GraphAccess::Write
                } else {
                    brainwire_bolt::auth::GraphAccess::Read
                }
            );
            tokens.push(token.to_owned());
        }
        assert_ne!(tokens[0], tokens[1]);
        assert_eq!(
            std::fs::metadata(directory.join("policy.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let original = std::fs::read(directory.join("admin.json")).unwrap();
        assert!(initialize(&directory, "tickets").is_err());
        assert_eq!(
            std::fs::read(directory.join("admin.json")).unwrap(),
            original
        );
    }

    #[test]
    fn invalid_graph_does_not_create_files() {
        let temporary = tempfile::tempdir().unwrap();
        for graph in ["", "../other", "default graph", &"x".repeat(129)] {
            let directory = temporary.path().join("auth");
            assert!(initialize(&directory, graph).is_err());
            assert!(!directory.exists());
        }
    }
}
