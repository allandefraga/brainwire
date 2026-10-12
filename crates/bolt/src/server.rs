//! Accepts TCP connections, negotiates the version, then feeds messages to a
//! Session and writes its responses back.

use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;

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
pub const DEFAULT_MAX_CONNECTIONS: usize = 64;

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
    serve_with_auth_and_limit(listener, make_engine, max_message_bytes, AuthMode::AllowAll).await
}

/// Serve with an explicit authentication policy. Protected engines must accept
/// authenticated access contexts; this transport does not provide native TLS.
pub async fn serve_with_auth_and_limit<E, F>(
    listener: TcpListener,
    make_engine: F,
    max_message_bytes: usize,
    auth: AuthMode,
) -> std::io::Result<()>
where
    E: QueryEngine + Send + 'static,
    F: Fn() -> E + Send + Sync + 'static,
{
    let make_engine = Arc::new(make_engine);
    let permits = Arc::new(Semaphore::new(DEFAULT_MAX_CONNECTIONS));
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(err) => {
                // A single bad `accept()` must never end the process: with a
                // low file-descriptor ulimit and enough idle connections that
                // never send the handshake, this was previously reached
                // (EMFILE) and the `?` below took the whole server down,
                // refusing every new client from then on. Log and keep
                // serving instead.
                eprintln!("brainwire-bolt: accept() failed: {err}");
                if is_resource_exhausted(&err) {
                    // The shortage will not clear itself between this
                    // `accept()` and the next: spinning the loop as fast as
                    // possible only burns CPU (and floods this log) while
                    // existing connections close and free the resource up.
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                continue;
            }
        };
        let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
            drop(stream);
            continue;
        };
        let make = Arc::clone(&make_engine);
        let auth = auth.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let _ = handle_connection(stream, make(), max_message_bytes, auth).await;
        });
    }
}

/// True when an `accept()` error means the process (or the whole system) is
/// out of some finite resource -- file descriptors, the socket backlog, or
/// kernel memory -- rather than a problem specific to one connection
/// attempt (for example `ECONNABORTED` from a peer that reset before the
/// handshake finished). [`serve_with_limit`] backs off briefly for these
/// instead of spinning the accept loop as fast as the kernel will let it
/// fail, which only burns CPU while the shortage persists; anything else is
/// safe, and better, to retry at once.
fn is_resource_exhausted(err: &std::io::Error) -> bool {
    // `ENOMEM` has a portable `ErrorKind` (stable since Rust 1.68). EMFILE,
    // ENFILE and ENOBUFS do not, so those are matched by raw errno instead,
    // which is why the numeric values need `cfg`: ENOBUFS differs between
    // macOS (55) and Linux (105) even though EMFILE/ENFILE/ENOMEM agree.
    if err.kind() == std::io::ErrorKind::OutOfMemory {
        return true;
    }
    #[cfg(target_os = "macos")]
    const EXHAUSTED: [i32; 3] = [
        24, /* EMFILE */
        23, /* ENFILE */
        55, /* ENOBUFS */
    ];
    #[cfg(target_os = "linux")]
    const EXHAUSTED: [i32; 3] = [
        24,  /* EMFILE */
        23,  /* ENFILE */
        105, /* ENOBUFS */
    ];
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    const EXHAUSTED: [i32; 0] = [];

    matches!(err.raw_os_error(), Some(code) if EXHAUSTED.contains(&code))
}

async fn handle_connection<E: QueryEngine + Send + 'static, S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    engine: E,
    max_message_bytes: usize,
    auth: AuthMode,
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
    let mut session = Session::new(engine, auth);
    let mut reader = ChunkReader::default();
    let mut buf = vec![0u8; 64 * 1024];
    let mut pending = 0usize;
    loop {
        while let Some(payload) = reader.next_message() {
            pending = 0;
            let mut input = payload.as_slice();
            match unpack(&mut input).ok().and_then(|v| parse_request(v).ok()) {
                Some(req) => {
                    // RocksDB calls are synchronous. Keep them off Tokio's IO workers.
                    let (next, responses) = tokio::task::spawn_blocking(move || {
                        let mut session = session;
                        let responses = session.handle(req);
                        (session, responses)
                    })
                    .await
                    .map_err(|err| std::io::Error::other(format!("query worker failed: {err}")))?;
                    session = next;
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
                    session.close();
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
            session.close();
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
async fn write_failure<S: AsyncWrite + Unpin>(
    stream: &mut S,
    code: &str,
    message: &str,
) -> std::io::Result<()> {
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
    use tokio::net::TcpStream;

    // I2 (review finding, 2026-09-25): `is_resource_exhausted` decides
    // whether an `accept()` error is a finite-resource shortage (worth a
    // brief backoff so the accept loop does not spin) or a per-connection
    // problem (safe, and better, to retry at once).

    #[test]
    fn out_of_memory_is_resource_exhaustion_on_every_platform() {
        let err = std::io::Error::from(std::io::ErrorKind::OutOfMemory);
        assert!(is_resource_exhausted(&err));
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn emfile_and_enfile_are_resource_exhaustion() {
        assert!(is_resource_exhausted(&std::io::Error::from_raw_os_error(
            24
        ))); // EMFILE
        assert!(is_resource_exhausted(&std::io::Error::from_raw_os_error(
            23
        ))); // ENFILE
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn enobufs_is_resource_exhaustion_with_the_macos_errno() {
        assert!(is_resource_exhausted(&std::io::Error::from_raw_os_error(
            55
        )));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn enobufs_is_resource_exhaustion_with_the_linux_errno() {
        assert!(is_resource_exhausted(&std::io::Error::from_raw_os_error(
            105
        )));
    }

    #[test]
    fn a_connection_level_error_is_not_resource_exhaustion() {
        // ECONNABORTED: a peer reset before the handshake finished. Nothing
        // about the server's own resources is wrong, so the caller should
        // retry accept() immediately instead of sleeping.
        let err = std::io::Error::from(std::io::ErrorKind::ConnectionAborted);
        assert!(!is_resource_exhausted(&err));
    }

    async fn handshake<S: AsyncRead + AsyncWrite + Unpin>(client: &mut S) {
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
    async fn reset_right_after_the_handshake_gets_one_failure_then_the_connection_closes() {
        // I1 (review finding, 2026-09-25): over a real socket, handshake then
        // RESET must not silently reach READY (an authentication bypass). It
        // must answer exactly one FAILURE and then close -- and a RUN sent
        // right behind it must never be answered, because the server has
        // already gone DEFUNCT and stopped serving the connection.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve(listener, || StubEngine));

        let mut client = TcpStream::connect(addr).await.unwrap();
        handshake(&mut client).await;

        let mut reset_body = Vec::new();
        pack(
            &PackValue::Struct {
                tag: 0x0F,
                fields: vec![],
            },
            &mut reset_body,
        );
        client
            .write_all(&encode_message(&reset_body))
            .await
            .unwrap();

        let mut run_body = Vec::new();
        pack(
            &PackValue::Struct {
                tag: 0x10,
                fields: vec![
                    PackValue::String("RETURN 7 AS z".into()),
                    PackValue::Map(vec![]),
                    PackValue::Map(vec![]),
                ],
            },
            &mut run_body,
        );
        // The server may already have closed its side by the time this
        // arrives; whether the write itself errors is not the point being
        // tested, so it is not unwrapped.
        let _ = client.write_all(&encode_message(&run_body)).await;

        let mut out = Vec::new();
        let read = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.read_to_end(&mut out),
        )
        .await;
        assert!(read.is_ok(), "server never closed the connection");
        let (code, _message) = decode_failure(&out);
        assert_eq!(code, "Neo.ClientError.Request.Invalid");
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
    struct CleanupEngine(Arc<tokio::sync::Notify>);
    impl QueryEngine for CleanupEngine {
        fn begin(&mut self, _: Option<&str>) -> Result<(), crate::session::EngineError> {
            Ok(())
        }
        fn run(
            &mut self,
            _: Option<&str>,
            _: &str,
            _: &PackValue,
        ) -> Result<crate::session::QueryResult, crate::session::EngineError> {
            unreachable!()
        }
        fn commit(&mut self) -> Result<Option<String>, crate::session::EngineError> {
            unreachable!()
        }
        fn abort(&mut self) -> Result<(), crate::session::EngineError> {
            self.0.notify_one();
            Ok(())
        }
    }

    async fn send_request<S: AsyncWrite + Unpin>(client: &mut S, tag: u8, fields: Vec<PackValue>) {
        let mut bytes = Vec::new();
        pack(&PackValue::Struct { tag, fields }, &mut bytes);
        client.write_all(&encode_message(&bytes)).await.unwrap();
    }

    async fn read_success<S: AsyncRead + Unpin>(client: &mut S) {
        let mut body = Vec::new();
        loop {
            let n = client.read_u16().await.unwrap() as usize;
            if n == 0 {
                break;
            }
            let start = body.len();
            body.resize(start + n, 0);
            client.read_exact(&mut body[start..]).await.unwrap();
        }
        assert!(matches!(
            unpack(&mut body.as_slice()).unwrap(),
            PackValue::Struct { tag: 0x70, .. }
        ));
    }

    async fn login_and_begin<S: AsyncRead + AsyncWrite + Unpin>(client: &mut S) {
        handshake(client).await;
        for tag in [0x01, 0x6a, 0x11] {
            send_request(client, tag, vec![PackValue::Map(vec![])]).await;
            read_success(client).await;
        }
    }

    #[tokio::test]
    async fn malformed_and_oversize_cleanup_precedes_a_blocked_failure_write() {
        for oversized in [false, true] {
            // A bounded transport makes the FAILURE write deterministically block
            // until the peer reads; cleanup must already have happened.
            let (server, mut client) = tokio::io::duplex(32);
            let aborted = Arc::new(tokio::sync::Notify::new());
            let task = tokio::spawn(handle_connection(
                server,
                CleanupEngine(aborted.clone()),
                64,
                AuthMode::AllowAll,
            ));
            login_and_begin(&mut client).await;
            if oversized {
                client.write_all(&[0u8; 65]).await.unwrap();
            } else {
                client.write_all(&encode_message(&[0xe0])).await.unwrap();
            }
            tokio::time::timeout(std::time::Duration::from_secs(2), aborted.notified())
                .await
                .expect("abort before peer reads failure");
            assert!(
                !task.is_finished(),
                "failure must still be waiting for the peer"
            );
            let mut failure = Vec::new();
            client.read_to_end(&mut failure).await.unwrap();
            decode_failure(&failure);
            task.await.unwrap().unwrap();
        }
    }

    #[tokio::test]
    async fn socket_eof_aborts_a_pending_transaction() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let aborted = Arc::new(tokio::sync::Notify::new());
        let engine = CleanupEngine(aborted.clone());
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_connection(stream, engine, 1024, AuthMode::AllowAll)
                .await
                .unwrap();
        });
        let mut client = TcpStream::connect(address).await.unwrap();
        login_and_begin(&mut client).await;
        drop(client);
        tokio::time::timeout(std::time::Duration::from_secs(2), aborted.notified())
            .await
            .unwrap();
        task.await.unwrap();
    }
    struct AuthEngine(Arc<std::sync::Mutex<Vec<crate::auth::AccessContext>>>);
    impl QueryEngine for AuthEngine {
        fn set_access(
            &mut self,
            access: crate::auth::AccessContext,
        ) -> Result<(), crate::session::EngineError> {
            self.0.lock().unwrap().push(access);
            Ok(())
        }
        fn begin(&mut self, _: Option<&str>) -> Result<(), crate::session::EngineError> {
            unreachable!()
        }
        fn run(
            &mut self,
            _: Option<&str>,
            _: &str,
            _: &PackValue,
        ) -> Result<crate::session::QueryResult, crate::session::EngineError> {
            unreachable!()
        }
        fn commit(&mut self) -> Result<Option<String>, crate::session::EngineError> {
            unreachable!()
        }
        fn abort(&mut self) -> Result<(), crate::session::EngineError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn protected_listener_applies_policy_to_each_connection() {
        use crate::auth::{AccessContext, AuthPolicy, GraphAccess};
        let policy=AuthPolicy::from_json(br#"{"principals":[{"id":"reader","token_sha256":"a8ae6e6ee929abea3afcfc5258c8ccd6f85273e0d4626d26c7279f3250f77c8e","graphs":{"tickets":"read"},"instance_admin":false}]}"#).unwrap();
        let access = Arc::new(std::sync::Mutex::new(Vec::new()));
        let received = access.clone();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(serve_with_auth_and_limit(
            listener,
            move || AuthEngine(received.clone()),
            1024,
            AuthMode::Required(Arc::new(policy)),
        ));
        for valid in [false, true] {
            let mut client = TcpStream::connect(address).await.unwrap();
            handshake(&mut client).await;
            send_request(&mut client, 0x01, vec![PackValue::Map(vec![])]).await;
            read_success(&mut client).await;
            let auth = if valid {
                PackValue::Map(vec![
                    ("scheme".into(), PackValue::String("basic".into())),
                    ("principal".into(), PackValue::String("reader".into())),
                    (
                        "credentials".into(),
                        PackValue::String(
                            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                                .into(),
                        ),
                    ),
                ])
            } else {
                PackValue::Map(vec![])
            };
            send_request(&mut client, 0x6a, vec![auth]).await;
            if valid {
                read_success(&mut client).await;
                send_request(&mut client, 0x02, vec![]).await;
            }
            let mut out = Vec::new();
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                client.read_to_end(&mut out),
            )
            .await
            .unwrap()
            .unwrap();
            if valid {
                assert!(out.is_empty());
            } else {
                assert_eq!(
                    decode_failure(&out).0,
                    "Neo.ClientError.Security.Unauthorized"
                );
            }
        }
        server.abort();
        let _ = server.await;
        let access = access.lock().unwrap();
        assert_eq!(access.len(), 2);
        assert_eq!(access[0], AccessContext::Unauthenticated);
        assert!(
            matches!(&access[1],AccessContext::Authenticated(p) if p.id=="reader" && p.graphs.get("tickets")==Some(&GraphAccess::Read))
        );
    }
}
