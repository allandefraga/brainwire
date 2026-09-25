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

/// Caps how many bytes of a single Bolt message the server will buffer before
/// giving up on it. `ChunkReader` has no size limit of its own: a hostile or
/// merely buggy client that keeps sending complete chunks but never the
/// zero-size terminator would otherwise grow its internal buffers without
/// bound. Real Bolt messages in this minimal server (HELLO/LOGON extras, a
/// Cypher query and its parameters) are a few hundred bytes at most, so 1 MiB
/// is generous headroom, not a realistic ceiling.
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

pub async fn serve<E, F>(listener: TcpListener, make_engine: F) -> std::io::Result<()>
where
    E: QueryEngine + Send + 'static,
    F: Fn() -> E + Send + Sync + 'static,
{
    let make_engine = Arc::new(make_engine);
    loop {
        let (stream, _) = listener.accept().await?;
        let make = Arc::clone(&make_engine);
        tokio::spawn(async move {
            let _ = handle_connection(stream, make()).await;
        });
    }
}

async fn handle_connection<E: QueryEngine>(
    mut stream: TcpStream,
    engine: E,
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
            let responses = match unpack(&mut input).ok().and_then(|v| parse_request(v).ok()) {
                Some(req) => session.handle(req),
                None => vec![Response::Failure {
                    code: "Neo.ClientError.Request.Invalid".into(),
                    message: "malformed message".into(),
                }],
            };
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
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            return Ok(());
        }
        pending += n;
        if pending > MAX_MESSAGE_BYTES {
            let mut body = Vec::new();
            pack(
                &Response::Failure {
                    code: "Neo.ClientError.Request.Invalid".into(),
                    message: "message exceeds the maximum size".into(),
                }
                .to_value(),
                &mut body,
            );
            // Best-effort: the client may not be reading anymore. Either way
            // the connection ends here instead of buffering further.
            let _ = stream.write_all(&encode_message(&body)).await;
            return Ok(());
        }
        reader.push(&buf[..n]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::StubEngine;

    #[tokio::test]
    async fn a_message_that_never_terminates_is_capped_instead_of_buffered_forever() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve(listener, || StubEngine));

        let mut client = TcpStream::connect(addr).await.unwrap();
        let mut hello = MAGIC.to_vec();
        hello.extend_from_slice(&[0, 0, 4, 5]); // propose exactly 5.4
        hello.extend_from_slice(&[0u8; 12]); // remaining proposal slots, unused
        client.write_all(&hello).await.unwrap();
        let mut handshake_response = [0u8; 4];
        client.read_exact(&mut handshake_response).await.unwrap();
        assert_eq!(handshake_response, [0, 0, 4, 5]);

        // Flood complete 64 KiB chunks without ever sending the zero-size
        // terminator, so no message ever completes. A server that buffers
        // this without a cap would grow forever; ours must give up instead.
        let chunk_header = [0xFFu8, 0xFF]; // chunk size 0xFFFF, the max
        let chunk_body = vec![0u8; 0xFFFF];
        let mut sent = 0usize;
        while sent <= MAX_MESSAGE_BYTES {
            client.write_all(&chunk_header).await.unwrap();
            client.write_all(&chunk_body).await.unwrap();
            sent += chunk_body.len();
        }

        // The server must close the connection instead of buffering forever.
        let mut out = Vec::new();
        let read = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.read_to_end(&mut out),
        )
        .await;
        assert!(read.is_ok(), "server never closed the oversized connection");
        assert!(!out.is_empty(), "expected a FAILURE frame before the close");
        assert_eq!(
            &out[out.len() - 2..],
            &[0, 0],
            "response is chunk-terminated"
        );
    }
}
