//! Credentials shared by the native Bolt clients; never printed or passed in argv.
use std::io::Read;

use brainwire_bolt::packstream::PackValue;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Credentials {
    principal: String,
    credentials: String,
}

impl Credentials {
    pub(crate) fn load() -> Result<Option<Self>, String> {
        let Some(path) = std::env::var_os("BRAINWIRE_BOLT_AUTH_FILE") else {
            return Ok(None);
        };
        if path.is_empty() {
            return Err("BRAINWIRE_BOLT_AUTH_FILE must name a credentials file".into());
        }
        Self::load_file(std::path::Path::new(&path)).map(Some)
    }

    fn load_file(path: &std::path::Path) -> Result<Self, String> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        // A credential FIFO must not block a health probe waiting for a writer.
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK);
        }
        let file = options
            .open(path)
            .map_err(|_| "cannot open Bolt credentials file")?;
        if !file
            .metadata()
            .map_err(|_| "cannot inspect Bolt credentials file")?
            .is_file()
        {
            return Err("Bolt credentials must be a regular file".into());
        }
        let mut bytes = Vec::new();
        file.take(65_537)
            .read_to_end(&mut bytes)
            .map_err(|_| "cannot read Bolt credentials file")?;
        Self::parse(&bytes)
    }

    fn parse(bytes: &[u8]) -> Result<Self, String> {
        let invalid = || "invalid Bolt credentials file".to_string();
        if bytes.len() > 65_536 {
            return Err(invalid());
        }
        let credentials: Self = serde_json::from_slice(bytes).map_err(|_| invalid())?;
        if credentials.principal.is_empty()
            || credentials.principal.len() > 128
            || !credentials
                .principal
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            || credentials.credentials.len() != 64
            || !credentials
                .credentials
                .bytes()
                .all(|b| b.is_ascii_hexdigit())
        {
            return Err(invalid());
        }
        Ok(credentials)
    }

    pub(crate) fn logon(&self) -> PackValue {
        PackValue::Map(vec![
            ("scheme".into(), PackValue::String("basic".into())),
            (
                "principal".into(),
                PackValue::String(self.principal.clone()),
            ),
            (
                "credentials".into(),
                PackValue::String(self.credentials.clone()),
            ),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credentials_require_a_bounded_regular_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("credentials.json");
        let valid = format!(
            r#"{{"principal":"reader","credentials":"{}"}}"#,
            "a".repeat(64)
        );
        std::fs::write(&path, valid).unwrap();
        assert!(Credentials::load_file(&path).is_ok());
        #[cfg(unix)]
        {
            let link = directory.path().join("mounted-credentials");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert!(Credentials::load_file(&link).is_ok());
            assert!(Credentials::load_file(std::path::Path::new("/dev/null")).is_err());
        }
        assert!(Credentials::load_file(directory.path()).is_err());
        std::fs::write(&path, "x".repeat(65_537)).unwrap();
        assert!(Credentials::load_file(&path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn credentials_refuse_a_fifo_without_waiting_for_a_writer() {
        use std::os::unix::ffi::OsStrExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("credentials-fifo");
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: name is a valid NUL-terminated path inside the test directory.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(Credentials::load_file(&path).is_err());
    }

    #[test]
    fn credentials_are_strict_bounded_and_errors_do_not_echo_secrets() {
        let token = "a".repeat(64);
        let valid = format!(r#"{{"principal":"reader","credentials":"{token}"}}"#);
        assert!(Credentials::parse(valid.as_bytes()).is_ok());
        for malformed in [
            format!(r#"{{"principal":"reader","credentials":"{token}","extra":1}}"#),
            format!(r#"{{"principal":"reader","credentials":"{token}","credentials":"{token}"}}"#),
            r#"{"principal":"reader","credentials":"secret-marker"}"#.to_string(),
            "x".repeat(65_537),
        ] {
            let error = Credentials::parse(malformed.as_bytes()).err().unwrap();
            assert!(!error.contains("secret-marker") && !error.contains(&token));
        }
    }
}
