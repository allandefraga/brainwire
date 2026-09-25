//! Accepts TCP connections, negotiates the version, then feeds messages to a
//! Session and writes its responses back.

use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::chunk::{encode_message, ChunkReader};
use crate::handshake::{negotiate, response, MAGIC};
use crate::message::{parse_request, Response};
use crate::packstream::{pack, unpack};
use crate::session::{AuthMode, QueryEngine, Session, State};

/// Default cap on how many bytes of a single Bolt message the server will
/// buffer before giving up on it. `ChunkReader` has no size limit of its
/// own: a hostile or merely buggy client that keeps sending complete chunks
/// but never the zero-size terminator would otherwise grow its internal
/// buffers without bound. The platform's own bulk writes set the realistic
/// ceiling: a 250,000-row batch reaches about 200 MB in a single parameter,
/// so 256 MiB leaves headroom above that while still bounding memory. Use
/// [`serve_with_limit`] to override it.
pub const DEFAULT_MAX_MESSAGE_BYTES: usize = 256 * 1024 * 1024;

pub async fn serve<E, F>(listener: TcpListener, make_engine: F) -> std::io::Result<()>
where
    E: QueryEngine + Send + 'static,
    F: Fn() -> E + Send + Sync + 'static,
{
    serve_with_limit(listener, make_engine, DEFAULT_MAX_MESSAGE_BYTES).await
}

/// Same as [`serve`], but with an explicit cap on a single message's size
/// instead of [`DEFAULT_MAX_MESSAGE_BYTES`]. Mainly for tests, which use a
/// small limit so they do not need to allocate or transmit hundreds of MiB
/// to prove the cap works.
pub async fn serve_with_limit<E, F>(
    listener: TcpListener,
    make_engine: F,
    max_message_bytes: usize,
) -> std::io::Result<()>
where
    E: QueryEngine + Send + 'static,
    F: Fn() -> E + Send + Sync + 'static,
{
    let make_engine = Arc::new(make_engine);
    loop {
        let (stream, _) = listener.accept().await?;
        let make = Arc::clone(&make_engine);
        tokio::spawn(async move {
            let _ = handle_connection(stream, make(), max_message_bytes).await;
        });
    }
}

async fn handle_connection<E: QueryEngine>(
    mut stream: TcpStream,
    engine: E,
    max_message_bytes: usize,
) -> std::io::Result<()> {
    let mut hello = [0u8; 20];
    stream.read_exact(&mut hello).await?;
    if hello[..4] != MAGIC {
        return Ok(());
    }
    let proposals: [u8; 16] = hello[4..].try_into().expect("16 bytes");
    let version = negotiate(&proposals);
    stream.write_all(&response(version)).await?;
    if version.is_none() {
        return Ok(());
    }
    let mut session = Session::new(engine, AuthMode::AllowAll);
    let mut reader = ChunkReader::default();
    let mut buf = vec![0u8; 64 * 1024];
    let mut pending = 0usize;
    loop {
        while let Some(payload) = reader.next_message() {
            pending = 0;
            let mut input = payload.as_slice();
            match unpack(&mut input).ok().and_then(|v| parse_request(v).ok()) {
                Some(req) => {
                    let responses = session.handle(req);
                    let mut out = Vec::new();
                    for r in responses {
                        let mut body = Vec::new();
                        pack(&r.to_value(), &mut body);
                        out.extend_from_slice(&encode_message(&body));
                    }
                    if !out.is_empty() {
                        stream.write_all(&out).await?;
                    }
                    if session.state() == State::Defunct {
                        return Ok(());
                    }
                }
                None => {
                    // A message the server cannot even parse is a protocol
                    // violation, which is fatal (research/bolt.md section 3:
                    // the server goes to DEFUNCT and closes the connection).
                    // Tell the client why, then close instead of carrying on
                    // with a session that never saw the failure.
                    write_failure(
                        &mut stream,
                        "Neo.ClientError.Request.Invalid",
                        "malformed message",
                    )
                    .await?;
                    return Ok(());
                }
            }
        }
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            return Ok(());
        }
        pending += n;
        if pending > max_message_bytes {
            write_failure(
                &mut stream,
                "Neo.ClientError.Request.Invalid",
                &format!("message larger than {max_message_bytes} bytes"),
            )
            .await?;
            return Ok(());
        }
        reader.push(&buf[..n]);
    }
}

/// Packs a FAILURE, chunk-encodes it and writes it to `stream`.
async fn write_failure(stream: &mut TcpStream, code: &str, message: &str) -> std::io::Result<()> {
    let mut body = Vec::new();
    pack(
        &Response::Failure {
            code: code.to_string(),
            message: message.to_string(),
        }
        .to_value(),
        &mut body,
    );
    stream.write_all(&encode_message(&body)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packstream::PackValue;
    use crate::session::StubEngine;

    async fn handshake(client: &mut TcpStream) {
        let mut hello = MAGIC.to_vec();
        hello.extend_from_slice(&[0, 0, 4, 5]); // propose exactly 5.4
        hello.extend_from_slice(&[0u8; 12]); // remaining proposal slots, unused
        client.write_all(&hello).await.unwrap();
        let mut handshake_response = [0u8; 4];
        client.read_exact(&mut handshake_response).await.unwrap();
        assert_eq!(handshake_response, [0, 0, 4, 5]);
    }

    /// Decodes the single chunk-framed FAILURE expected in `bytes` (read
    /// from a connection after the server closed it), returning its
    /// `(code, message)`. Panics if there is not exactly one well-formed
    /// FAILURE.
    fn decode_failure(bytes: &[u8]) -> (String, String) {
        let mut reader = ChunkReader::default();
        reader.push(bytes);
        let payload = reader.next_message().expect("a framed message");
        assert!(
            reader.next_message().is_none(),
            "expected exactly one message before the close"
        );
        let mut input = payload.as_slice();
        match unpack(&mut input).expect("valid PackStream") {
            PackValue::Struct { tag: 0x7F, fields } => {
                let code = match fields[0].get("code") {
                    Some(PackValue::String(s)) => s.clone(),
                    other => panic!("expected a string code, got {other:?}"),
                };
                let message = match fields[0].get("message") {
                    Some(PackValue::String(s)) => s.clone(),
                    other => panic!("expected a string message, got {other:?}"),
                };
                (code, message)
            }
            other => panic!("expected a FAILURE struct (tag 0x7F), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_message_that_never_terminates_is_capped_instead_of_buffered_forever() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let limit = 1024usize;
        tokio::spawn(serve_with_limit(listener, || StubEngine, limit));

        let mut client = TcpStream::connect(addr).await.unwrap();
        handshake(&mut client).await;

        // Flood complete chunks without ever sending the zero-size
        // terminator, so no message ever completes. A server that buffers
        // this without a cap would grow forever; ours must give up once
        // `limit` is crossed. A small limit and small chunks keep this test
        // from allocating or transmitting real bulk-write-sized data.
        let chunk_body = vec![0u8; 300];
        let chunk_header = (chunk_body.len() as u16).to_be_bytes();
        let mut sent = 0usize;
        while sent <= limit {
            client.write_all(&chunk_header).await.unwrap();
            client.write_all(&chunk_body).await.unwrap();
            sent += chunk_body.len();
        }

        // The server must close the connection instead of buffering
        // forever, and say why.
        let mut out = Vec::new();
        let read = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.read_to_end(&mut out),
        )
        .await;
        assert!(read.is_ok(), "server never closed the oversized connection");
        let (code, message) = decode_failure(&out);
        assert_eq!(code, "Neo.ClientError.Request.Invalid");
        assert!(
            message.contains(&limit.to_string()),
            "message should name the limit that was crossed: {message}"
        );
    }

    #[tokio::test]
    async fn a_malformed_message_gets_a_failure_then_the_connection_closes() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve(listener, || StubEngine));

        let mut client = TcpStream::connect(addr).await.unwrap();
        handshake(&mut client).await;

        // 0xE0 is an unknown PackStream marker (see
        // packstream::tests::truncated_and_unknown): a well-framed message
        // whose payload the server cannot parse at all.
        client.write_all(&encode_message(&[0xE0])).await.unwrap();

        let mut out = Vec::new();
        let read = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.read_to_end(&mut out),
        )
        .await;
        assert!(
            read.is_ok(),
            "server never closed after the malformed message"
        );
        let (code, message) = decode_failure(&out);
        assert_eq!(code, "Neo.ClientError.Request.Invalid");
        assert_eq!(message, "malformed message");
    }
}
