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
    let address: SocketAddr = address
        .parse()
        .map_err(|e| format!("invalid probe address: {e}"))?;
    let timeout = Duration::from_millis(timeout_ms);
    let mut stream = TcpStream::connect_timeout(&address, timeout)
        .map_err(|e| format!("cannot connect to {address}: {e}"))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;

    let mut greeting = [0u8; 20];
    greeting[..4].copy_from_slice(&MAGIC);
    greeting[4..8].copy_from_slice(&[0, 0, 4, 5]);
    stream
        .write_all(&greeting)
        .map_err(|e| format!("handshake write: {e}"))?;
    let mut version = [0u8; 4];
    stream
        .read_exact(&mut version)
        .map_err(|e| format!("handshake read: {e}"))?;
    if version != [0, 0, 4, 5] {
        return Err(format!("unexpected Bolt version response: {version:?}"));
    }

    send(&mut stream, 0x01, vec![PackValue::Map(vec![])])?;
    expect_success(receive(&mut stream)?)?;
    send(&mut stream, 0x6a, vec![PackValue::Map(vec![])])?;
    expect_success(receive(&mut stream)?)?;

    if matches!(mode, Mode::Ready) {
        send(
            &mut stream,
            0x10,
            vec![
                PackValue::String("RETURN 1 AS ready".into()),
                PackValue::Map(vec![]),
                PackValue::Map(vec![]),
            ],
        )?;
        expect_success(receive(&mut stream)?)?;
        send(&mut stream, 0x3f, vec![PackValue::Map(vec![])])?;
        match receive(&mut stream)? {
            PackValue::Struct { tag: 0x71, fields }
                if fields == vec![PackValue::List(vec![PackValue::Int(1)])] => {}
            other => return Err(format!("unexpected probe record: {other:?}")),
        }
        expect_success(receive(&mut stream)?)?;
    }
    Ok(())
}

fn send(stream: &mut TcpStream, tag: u8, fields: Vec<PackValue>) -> Result<(), String> {
    let mut body = Vec::new();
    pack(&PackValue::Struct { tag, fields }, &mut body);
    stream
        .write_all(&encode_message(&body))
        .map_err(|e| format!("Bolt request write: {e}"))
}

fn receive(stream: &mut TcpStream) -> Result<PackValue, String> {
    let mut body = Vec::new();
    let mut noops = 0;
    loop {
        let mut size = [0u8; 2];
        stream
            .read_exact(&mut size)
            .map_err(|e| format!("Bolt response read: {e}"))?;
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
        if body.len() + size > MAX_PROBE_RESPONSE {
            return Err("Bolt probe response exceeds 64 KiB".into());
        }
        let start = body.len();
        body.resize(start + size, 0);
        stream
            .read_exact(&mut body[start..])
            .map_err(|e| format!("Bolt response read: {e}"))?;
    }
    let mut input = body.as_slice();
    let value = unpack(&mut input).map_err(|e| format!("invalid Bolt response: {e}"))?;
    if !input.is_empty() {
        return Err("trailing bytes in Bolt response".into());
    }
    Ok(value)
}

fn expect_success(response: PackValue) -> Result<(), String> {
    match response {
        PackValue::Struct {
            tag: 0x70,
            fields: _,
        } => Ok(()),
        PackValue::Struct { tag: 0x7f, fields } => Err(format!("Bolt failure: {fields:?}")),
        other => Err(format!("unexpected Bolt response: {other:?}")),
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
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        assert!(receive(&mut stream).unwrap_err().contains("too many empty"));
        writer.join().unwrap();
    }
}
