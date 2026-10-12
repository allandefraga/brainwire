//! A small Bolt client for container and orchestrator health checks.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use brainwire_bolt::chunk::encode_message;
use brainwire_bolt::handshake::MAGIC;
use brainwire_bolt::packstream::{pack, unpack, PackValue};
use clap::ValueEnum;

const MAX_PROBE_RESPONSE: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Mode {
    /// Complete a Bolt handshake and login without touching RocksDB.
    Live,
    /// Run a read-only query through the default database.
    Ready,
}

pub fn run(address: &str, mode: Mode, timeout_ms: u64) -> Result<(), String> {
    if !(1..=30_000).contains(&timeout_ms) {
        return Err("--timeout-ms must be between 1 and 30000".into());
    }
    let mut client = BoltClient::connect(address, timeout_ms, MAX_PROBE_RESPONSE)?;
    if matches!(mode, Mode::Ready) {
        client.send(
            0x10,
            vec![
                PackValue::String("RETURN 1 AS ready".into()),
                PackValue::Map(vec![]),
                PackValue::Map(vec![]),
            ],
        )?;
        success(client.receive()?)?;
        client.send(0x3f, vec![PackValue::Map(vec![])])?;
        match client.receive()? {
            PackValue::Struct { tag: 0x71, fields }
                if fields == vec![PackValue::List(vec![PackValue::Int(1)])] => {}
            other => return Err(format!("unexpected probe record: {other:?}")),
        }
        success(client.receive()?)?;
    }
    Ok(())
}

/// The two bounded, synchronous read-only clients share framing and deadlines.
pub(crate) struct BoltClient {
    stream: TcpStream,
    deadline: std::time::Instant,
    remaining_wire: usize,
}

impl BoltClient {
    pub(crate) fn connect(address: &str, timeout_ms: u64, max_wire: usize) -> Result<Self, String> {
        let address: SocketAddr = address
            .parse()
            .map_err(|e| format!("invalid Bolt address: {e}"))?;
        let timeout = Duration::from_millis(timeout_ms);
        let deadline = std::time::Instant::now() + timeout;
        let stream = TcpStream::connect_timeout(&address, timeout)
            .map_err(|e| format!("cannot connect to {address}: {e}"))?;
        let mut client = Self {
            stream,
            deadline,
            remaining_wire: max_wire,
        };
        let mut greeting = [0u8; 20];
        greeting[..4].copy_from_slice(&MAGIC);
        greeting[4..8].copy_from_slice(&[0, 0, 4, 5]);
        client.write(&greeting)?;
        let mut version = [0u8; 4];
        client.read(&mut version)?;
        if version != [0, 0, 4, 5] {
            return Err(format!("unexpected Bolt version response: {version:?}"));
        }
        client.send(0x01, vec![PackValue::Map(vec![])])?;
        success(client.receive()?)?;
        let credentials = crate::client_auth::Credentials::load()?;
        let auth = credentials
            .as_ref()
            .map(|credentials| credentials.logon())
            .unwrap_or(PackValue::Map(vec![]));
        client.send(0x6a, vec![auth])?;
        success(client.receive()?).map_err(|_| "Bolt authentication failed".to_string())?;
        Ok(client)
    }

    fn timeout(&self) -> Result<Duration, String> {
        self.deadline
            .checked_duration_since(std::time::Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(|| "Bolt request deadline exceeded".into())
    }

    fn write(&mut self, mut bytes: &[u8]) -> Result<(), String> {
        while !bytes.is_empty() {
            self.stream
                .set_write_timeout(Some(self.timeout()?))
                .map_err(|e| e.to_string())?;
            match self.stream.write(bytes) {
                Ok(0) => return Err("Bolt request write: connection closed".into()),
                Ok(n) => bytes = &bytes[n..],
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) =>
                {
                    return Err("Bolt request deadline exceeded".into());
                }
                Err(e) => return Err(format!("Bolt request write: {e}")),
            }
        }
        self.timeout()?;
        Ok(())
    }

    fn read(&mut self, mut bytes: &mut [u8]) -> Result<(), String> {
        if bytes.len() > self.remaining_wire {
            return Err("Bolt response exceeds wire limit".into());
        }
        while !bytes.is_empty() {
            self.stream
                .set_read_timeout(Some(self.timeout()?))
                .map_err(|e| e.to_string())?;
            match self.stream.read(bytes) {
                Ok(0) => return Err("Bolt response read: connection closed".into()),
                Ok(n) => {
                    self.remaining_wire -= n;
                    bytes = &mut bytes[n..];
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) =>
                {
                    return Err("Bolt request deadline exceeded".into());
                }
                Err(e) => return Err(format!("Bolt response read: {e}")),
            }
        }
        self.timeout()?;
        Ok(())
    }

    pub(crate) fn send(&mut self, tag: u8, fields: Vec<PackValue>) -> Result<(), String> {
        let mut body = Vec::new();
        pack(&PackValue::Struct { tag, fields }, &mut body);
        self.write(&encode_message(&body))
    }

    pub(crate) fn receive(&mut self) -> Result<PackValue, String> {
        let mut body = Vec::new();
        let mut noops = 0;
        loop {
            let mut size = [0u8; 2];
            self.read(&mut size)?;
            let size = usize::from(u16::from_be_bytes(size));
            if size == 0 {
                if !body.is_empty() {
                    break;
                }
                noops += 1;
                if noops > 16 {
                    return Err("too many empty Bolt chunks".into());
                }
                continue;
            }
            if size > self.remaining_wire {
                return Err("Bolt response exceeds wire limit".into());
            }
            let start = body.len();
            body.resize(start + size, 0);
            self.read(&mut body[start..])?;
        }
        let mut input = body.as_slice();
        let value = unpack(&mut input).map_err(|e| format!("invalid Bolt response: {e}"))?;
        if !input.is_empty() {
            return Err("trailing bytes in Bolt response".into());
        }
        Ok(value)
    }

    pub(crate) fn finish(&mut self) -> Result<(), String> {
        // GOODBYE closes the server side, so delayed extra records cannot race a peek.
        self.send(0x02, vec![])?;
        loop {
            self.stream
                .set_read_timeout(Some(self.timeout()?))
                .map_err(|e| e.to_string())?;
            match self.stream.read(&mut [0u8; 1]) {
                Ok(0) => {
                    self.timeout()?;
                    return Ok(());
                }
                Ok(_) => return Err("trailing Bolt response after result completion".into()),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) =>
                {
                    return Err("Bolt request deadline exceeded".into());
                }
                Err(e) => return Err(format!("Bolt response completion: {e}")),
            }
        }
    }
}

pub(crate) fn success(response: PackValue) -> Result<PackValue, String> {
    match response {
        PackValue::Struct {
            tag: 0x70,
            mut fields,
        } if fields.len() == 1 => {
            let metadata = fields.remove(0);
            if let PackValue::Map(entries) = &metadata {
                let mut keys = std::collections::HashSet::new();
                if entries.iter().all(|(key, _)| keys.insert(key)) {
                    return Ok(metadata);
                }
            }
            Err("invalid Bolt SUCCESS metadata".into())
        }
        PackValue::Struct { tag: 0x7f, fields } => {
            let detail = fields.first().and_then(|metadata| metadata.get("message"));
            let message = match detail {
                Some(PackValue::String(message)) => message
                    .chars()
                    .take(400)
                    .flat_map(char::escape_default)
                    .collect::<String>(),
                _ => "server rejected request".into(),
            };
            Err(format!("Bolt failure: {message}"))
        }
        _ => Err("unexpected Bolt response (expected SUCCESS)".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn repeated_empty_chunks_cannot_hold_a_probe_open_forever() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let writer = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(&[0; 34]).unwrap();
        });
        let stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut client = BoltClient {
            stream,
            deadline: std::time::Instant::now() + Duration::from_secs(1),
            remaining_wire: MAX_PROBE_RESPONSE,
        };
        assert!(client.receive().unwrap_err().contains("too many empty"));
        writer.join().unwrap();
    }
}
