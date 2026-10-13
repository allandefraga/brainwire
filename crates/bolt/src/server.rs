//! Accepts TCP connections, negotiates the version, then feeds messages to a
//! Session and writes its responses back.

use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;

use crate::chunk::{encode_message, ChunkReader};
use crate::handshake::{negotiate, response, MAGIC};
use crate::message::{parse_request, Response};
#[cfg(test)]
use crate::packstream::unpack;
use crate::packstream::{pack, unpack_with_limit};
use crate::session::{AuthMode, QueryEngine, Session, State};

/// Default cap on assembled PackStream payload bytes per Bolt message,
/// excluding chunk framing and NOOPs. Announced chunks are checked even if
/// the client never sends their body or the message terminator.
/// The platform's own bulk writes set the realistic
/// ceiling: a 250,000-row batch reaches about 200 MB in a single parameter,
/// so 256 MiB leaves headroom above that while still bounding memory. Use
/// [`serve_with_limit`] to override it.
pub const DEFAULT_MAX_MESSAGE_BYTES: usize = 256 * 1024 * 1024;
/// Default quota for PackStream owned payloads and container backing storage.
pub const DEFAULT_MAX_DECODED_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
pub const DEFAULT_MAX_CONNECTIONS: usize = 64;
/// Absolute deadline for reading the handshake and writing its version reply.
pub const DEFAULT_HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Absolute IO budget until successful authentication, renewed after LOGOFF.
/// Synchronous parsing and engine work can postpone enforcement.
pub const DEFAULT_AUTHENTICATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Complete request and response IO budgets; synchronous query work is excluded.
#[derive(Debug, Clone, Copy)]
pub struct ConnectionLimits {
    pub max_message_bytes: usize,
    pub max_decoded_message_bytes: usize,
    pub io_timeout: std::time::Duration,
}

impl Default for ConnectionLimits {
    fn default() -> Self {
        Self {
            max_message_bytes: DEFAULT_MAX_MESSAGE_BYTES,
            max_decoded_message_bytes: DEFAULT_MAX_DECODED_MESSAGE_BYTES,
            io_timeout: std::time::Duration::from_secs(300),
        }
    }
}

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
    serve_with_auth_and_limits(
        listener,
        make_engine,
        max_message_bytes,
        DEFAULT_MAX_DECODED_MESSAGE_BYTES,
        auth,
    )
    .await
}

/// Serve with independent encoded-message and decoded-storage caps.
pub async fn serve_with_auth_and_limits<E, F>(
    listener: TcpListener,
    make_engine: F,
    max_message_bytes: usize,
    max_decoded_message_bytes: usize,
    auth: AuthMode,
) -> std::io::Result<()>
where
    E: QueryEngine + Send + 'static,
    F: Fn() -> E + Send + Sync + 'static,
{
    serve_with_connection_limits(
        listener,
        make_engine,
        ConnectionLimits {
            max_message_bytes,
            max_decoded_message_bytes,
            ..ConnectionLimits::default()
        },
        auth,
    )
    .await
}

/// Serve with encoded/decoded caps and absolute authenticated IO budgets.
pub async fn serve_with_connection_limits<E, F>(
    listener: TcpListener,
    make_engine: F,
    limits: ConnectionLimits,
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
            let _ = handle_connection_with_connection_timeouts(
                stream,
                make(),
                limits,
                auth,
                DEFAULT_HANDSHAKE_TIMEOUT,
                DEFAULT_AUTHENTICATION_TIMEOUT,
            )
            .await;
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

#[cfg(test)]
async fn handle_connection<E: QueryEngine + Send + 'static, S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    engine: E,
    max_message_bytes: usize,
    auth: AuthMode,
) -> std::io::Result<()> {
    handle_connection_with_timeouts(
        stream,
        engine,
        max_message_bytes,
        auth,
        DEFAULT_HANDSHAKE_TIMEOUT,
        DEFAULT_AUTHENTICATION_TIMEOUT,
    )
    .await
}

#[cfg(test)]
async fn handle_connection_with_timeouts<
    E: QueryEngine + Send + 'static,
    S: AsyncRead + AsyncWrite + Unpin,
>(
    stream: S,
    engine: E,
    max_message_bytes: usize,
    auth: AuthMode,
    handshake_timeout: std::time::Duration,
    authentication_timeout: std::time::Duration,
) -> std::io::Result<()> {
    handle_connection_with_limits_and_timeouts(
        stream,
        engine,
        max_message_bytes,
        DEFAULT_MAX_DECODED_MESSAGE_BYTES,
        auth,
        handshake_timeout,
        authentication_timeout,
    )
    .await
}

#[cfg(test)]
async fn handle_connection_with_limits_and_timeouts<
    E: QueryEngine + Send + 'static,
    S: AsyncRead + AsyncWrite + Unpin,
>(
    stream: S,
    engine: E,
    max_message_bytes: usize,
    max_decoded_message_bytes: usize,
    auth: AuthMode,
    handshake_timeout: std::time::Duration,
    authentication_timeout: std::time::Duration,
) -> std::io::Result<()> {
    handle_connection_with_connection_timeouts(
        stream,
        engine,
        ConnectionLimits {
            max_message_bytes,
            max_decoded_message_bytes,
            ..ConnectionLimits::default()
        },
        auth,
        handshake_timeout,
        authentication_timeout,
    )
    .await
}

async fn handle_connection_with_connection_timeouts<
    E: QueryEngine + Send + 'static,
    S: AsyncRead + AsyncWrite + Unpin,
>(
    mut stream: S,
    engine: E,
    limits: ConnectionLimits,
    auth: AuthMode,
    handshake_timeout: std::time::Duration,
    authentication_timeout: std::time::Duration,
) -> std::io::Result<()> {
    let negotiated = tokio::time::timeout(handshake_timeout, async {
        let mut hello = [0u8; 20];
        stream.read_exact(&mut hello).await?;
        if hello[..4] != MAGIC {
            return Ok::<_, std::io::Error>(false);
        }
        let proposals: [u8; 16] = hello[4..].try_into().expect("16 bytes");
        let version = negotiate(&proposals);
        stream.write_all(&response(version)).await?;
        Ok(version.is_some())
    })
    .await
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "Bolt handshake timed out"))??;
    if !negotiated {
        return Ok(());
    }
    let mut session = Session::new(engine, auth);
    let mut authentication_deadline = Some(tokio::time::Instant::now() + authentication_timeout);
    let mut request_deadline = None;
    let mut reader = ChunkReader::default();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        loop {
            check_io_deadline(&mut session, authentication_deadline.or(request_deadline))?;
            let payload = match reader.next_message_with_limit(limits.max_message_bytes) {
                Ok(Some(payload)) => payload,
                Ok(None) => break,
                Err(_) => {
                    session.close();
                    io_with_deadline(
                        &mut session,
                        authentication_deadline
                            .or_else(|| Some(tokio::time::Instant::now() + limits.io_timeout)),
                        write_failure(
                            &mut stream,
                            "Neo.ClientError.Request.Invalid",
                            &format!("message larger than {} bytes", limits.max_message_bytes),
                        ),
                    )
                    .await?;
                    return Ok(());
                }
            };
            let mut input = payload.as_slice();
            match unpack_with_limit(&mut input, limits.max_decoded_message_bytes)
                .ok()
                .filter(|_| input.is_empty())
                .and_then(|v| parse_request(v).ok())
            {
                Some(req) => {
                    let response_deadline = authentication_deadline;
                    check_io_deadline(&mut session, response_deadline.or(request_deadline))?;
                    // RocksDB calls are synchronous. Keep them off Tokio's IO workers.
                    let (next, responses) = tokio::task::spawn_blocking(move || {
                        let mut session = session;
                        let responses = session.handle(req);
                        (session, responses)
                    })
                    .await
                    .map_err(|err| std::io::Error::other(format!("query worker failed: {err}")))?;
                    session = next;
                    check_io_deadline(&mut session, response_deadline)?;
                    // LOGOFF starts a fresh budget before its SUCCESS is written.
                    if response_deadline.is_none() && session.state() == State::Authentication {
                        authentication_deadline =
                            Some(tokio::time::Instant::now() + authentication_timeout);
                    }
                    let write_deadline = authentication_deadline
                        .or_else(|| Some(tokio::time::Instant::now() + limits.io_timeout));
                    for r in responses {
                        check_io_deadline(&mut session, write_deadline)?;
                        let mut body = Vec::new();
                        pack(&r.to_value(), &mut body);
                        let frame = encode_message(&body);
                        io_with_deadline(&mut session, write_deadline, stream.write_all(&frame))
                            .await?;
                    }
                    check_io_deadline(&mut session, write_deadline)?;
                    if session.state() == State::Defunct {
                        return Ok(());
                    }
                    if !matches!(session.state(), State::Connected | State::Authentication) {
                        authentication_deadline = None;
                        request_deadline = Some(tokio::time::Instant::now() + limits.io_timeout);
                    } else {
                        request_deadline = None;
                    }
                }
                None => {
                    // A message the server cannot even parse is a protocol
                    // violation, which is fatal (research/bolt.md section 3:
                    // the server goes to DEFUNCT and closes the connection).
                    // Tell the client why, then close instead of carrying on
                    // with a session that never saw the failure.
                    session.close();
                    io_with_deadline(
                        &mut session,
                        authentication_deadline
                            .or_else(|| Some(tokio::time::Instant::now() + limits.io_timeout)),
                        write_failure(
                            &mut stream,
                            "Neo.ClientError.Request.Invalid",
                            "malformed message",
                        ),
                    )
                    .await?;
                    return Ok(());
                }
            }
        }
        let n = io_with_deadline(
            &mut session,
            authentication_deadline.or(request_deadline),
            stream.read(&mut buf),
        )
        .await?;
        if n == 0 {
            session.close();
            return Ok(());
        }
        reader.push(&buf[..n]);
    }
}

fn check_io_deadline<E: QueryEngine>(
    session: &mut Session<E>,
    deadline: Option<tokio::time::Instant>,
) -> std::io::Result<()> {
    if deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
        session.close();
        return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "Bolt connection IO timed out",
        ));
    }
    Ok(())
}

async fn io_with_deadline<E: QueryEngine, T>(
    session: &mut Session<E>,
    deadline: Option<tokio::time::Instant>,
    operation: impl std::future::Future<Output = std::io::Result<T>>,
) -> std::io::Result<T> {
    check_io_deadline(session, deadline)?;
    let result = match deadline {
        Some(deadline) => match tokio::time::timeout_at(deadline, operation).await {
            Ok(result) => result,
            Err(_) => {
                session.close();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "Bolt connection IO timed out",
                ));
            }
        },
        None => operation.await,
    };
    check_io_deadline(session, deadline)?;
    if result.is_err() {
        session.close();
    }
    result
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

    fn authentication_connection<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
        server: S,
    ) -> tokio::task::JoinHandle<std::io::Result<()>> {
        tokio::spawn(handle_connection_with_timeouts(
            server,
            StubEngine,
            64,
            AuthMode::AllowAll,
            std::time::Duration::from_secs(1),
            std::time::Duration::from_millis(40),
        ))
    }

    async fn authentication_timed_out(task: tokio::task::JoinHandle<std::io::Result<()>>) {
        let err = tokio::time::timeout(std::time::Duration::from_millis(200), task)
            .await
            .expect("authentication IO deadline")
            .unwrap()
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
    }

    #[tokio::test]
    async fn unauthenticated_idle_hello_noops_and_partial_logon_do_not_renew_deadline() {
        for mode in 0..4 {
            let (server, mut client) = tokio::io::duplex(1024);
            let task = authentication_connection(server);
            handshake(&mut client).await;
            if mode != 0 {
                send_request(&mut client, 0x01, vec![PackValue::Map(vec![])]).await;
                read_success(&mut client).await;
            }
            let drip = tokio::spawn(async move {
                if mode == 2 {
                    loop {
                        if client.write_all(&[0, 0]).await.is_err() {
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                } else if mode == 3 {
                    let bytes = encode_message(&[0xb1, 0x6a, 0xa0]);
                    for byte in bytes {
                        if client.write_all(&[byte]).await.is_err() {
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                } else {
                    std::future::pending::<()>().await;
                }
            });
            authentication_timed_out(task).await;
            drip.abort();
            let _ = drip.await;
        }
    }

    #[tokio::test]
    async fn login_clears_the_deadline_and_logoff_starts_a_fresh_one() {
        let (server, mut client) = tokio::io::duplex(1024);
        let task = authentication_connection(server);
        handshake(&mut client).await;
        for tag in [0x01, 0x6a] {
            send_request(&mut client, tag, vec![PackValue::Map(vec![])]).await;
            read_success(&mut client).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        assert!(!task.is_finished(), "authenticated idle has no deadline");
        send_request(&mut client, 0x6b, vec![]).await;
        read_success(&mut client).await;
        send_request(&mut client, 0x6a, vec![PackValue::Map(vec![])]).await;
        read_success(&mut client).await;
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        assert!(
            !task.is_finished(),
            "reauthentication clears the fresh deadline"
        );
        send_request(&mut client, 0x6b, vec![]).await;
        read_success(&mut client).await;
        authentication_timed_out(task).await;
        let mut bytes = Vec::new();
        client.read_to_end(&mut bytes).await.unwrap();
        assert!(bytes.is_empty(), "expiry sends no FAILURE");
    }

    fn protected_test_policy() -> crate::auth::AuthPolicy {
        use crate::auth::AuthPolicy;
        AuthPolicy::from_json(br#"{"principals":[{"id":"reader","token_sha256":"a8ae6e6ee929abea3afcfc5258c8ccd6f85273e0d4626d26c7279f3250f77c8e","graphs":{"tickets":"read"},"instance_admin":false}]}"#).unwrap()
    }

    #[tokio::test]
    async fn authentication_deadline_bounds_success_and_failure_writes() {
        for mode in [
            "hello",
            "logon",
            "malformed",
            "oversized",
            "logoff",
            "denied",
        ] {
            let (server, mut client) = tokio::io::duplex(1);
            let auth = if mode == "denied" {
                AuthMode::Required(Arc::new(protected_test_policy()))
            } else {
                AuthMode::AllowAll
            };
            let task = tokio::spawn(handle_connection_with_timeouts(
                server,
                StubEngine,
                64,
                auth,
                std::time::Duration::from_secs(1),
                std::time::Duration::from_millis(40),
            ));
            handshake(&mut client).await;
            if matches!(mode, "logon" | "logoff" | "denied") {
                send_request(&mut client, 0x01, vec![PackValue::Map(vec![])]).await;
                read_success(&mut client).await;
            }
            if mode == "logoff" {
                send_request(&mut client, 0x6a, vec![PackValue::Map(vec![])]).await;
                read_success(&mut client).await;
            }
            match mode {
                "hello" => send_request(&mut client, 0x01, vec![PackValue::Map(vec![])]).await,
                "logon" | "denied" => {
                    send_request(&mut client, 0x6a, vec![PackValue::Map(vec![])]).await
                }
                "logoff" => send_request(&mut client, 0x6b, vec![]).await,
                "malformed" => client.write_all(&encode_message(&[0xe0])).await.unwrap(),
                "oversized" => client.write_all(&[0, 65]).await.unwrap(),
                _ => unreachable!(),
            }
            authentication_timed_out(task).await;
        }
    }

    #[tokio::test]
    async fn authentication_timeout_releases_the_owned_connection_permit() {
        let permits = Arc::new(Semaphore::new(1));
        let permit = permits.clone().try_acquire_owned().unwrap();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let engine = BeginProbe(calls.clone());
        let (server, mut client) = tokio::io::duplex(1024);
        let task = tokio::spawn(async move {
            let _permit = permit;
            handle_connection_with_timeouts(
                server,
                engine,
                64,
                AuthMode::AllowAll,
                std::time::Duration::from_secs(1),
                std::time::Duration::from_millis(40),
            )
            .await
        });
        handshake(&mut client).await;
        assert!(permits.clone().try_acquire_owned().is_err());
        authentication_timed_out(task).await;
        assert!(permits.clone().try_acquire_owned().is_ok());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        let mut bytes = Vec::new();
        client.read_to_end(&mut bytes).await.unwrap();
        assert!(bytes.is_empty());
    }

    #[tokio::test]
    async fn silent_and_slow_drip_handshakes_time_out_and_release_the_permit() {
        for slow_drip in [false, true] {
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let permits = Arc::new(Semaphore::new(1));
            let permit = permits.clone().try_acquire_owned().unwrap();
            let (server, mut client) = tokio::io::duplex(1024);
            let engine = BeginProbe(calls.clone());
            let task = tokio::spawn(async move {
                let _permit = permit;
                handle_connection_with_timeouts(
                    server,
                    engine,
                    64,
                    AuthMode::AllowAll,
                    std::time::Duration::from_millis(50),
                    DEFAULT_AUTHENTICATION_TIMEOUT,
                )
                .await
            });
            assert!(permits.clone().try_acquire_owned().is_err());
            let drip = tokio::spawn(async move {
                if slow_drip {
                    for byte in MAGIC.into_iter().chain([0; 16]) {
                        if client.write_all(&[byte]).await.is_err() {
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                } else {
                    std::future::pending::<()>().await;
                }
            });
            let err = tokio::time::timeout(std::time::Duration::from_millis(200), task)
                .await
                .expect("absolute handshake deadline")
                .unwrap()
                .unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
            assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
            assert!(permits.clone().try_acquire_owned().is_ok());
            drip.abort();
            let _ = drip.await;
        }
    }

    #[tokio::test]
    async fn valid_handshake_with_a_deadline_keeps_established_sessions_open() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (server, mut client) = tokio::io::duplex(1024);
        let task = tokio::spawn(handle_connection_with_timeouts(
            server,
            BeginProbe(calls.clone()),
            64,
            AuthMode::AllowAll,
            std::time::Duration::from_millis(50),
            DEFAULT_AUTHENTICATION_TIMEOUT,
        ));
        handshake(&mut client).await;
        tokio::time::sleep(std::time::Duration::from_millis(75)).await;
        assert!(!task.is_finished(), "deadline ends after negotiation");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        drop(client);
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn handshake_deadline_includes_a_blocked_version_reply() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (server, mut client) = tokio::io::duplex(1);
        let task = tokio::spawn(handle_connection_with_timeouts(
            server,
            BeginProbe(calls.clone()),
            64,
            AuthMode::AllowAll,
            std::time::Duration::from_millis(50),
            DEFAULT_AUTHENTICATION_TIMEOUT,
        ));
        let mut hello = MAGIC.to_vec();
        hello.extend_from_slice(&[0, 0, 4, 5]);
        hello.extend_from_slice(&[0; 12]);
        client.write_all(&hello).await.unwrap();
        // The single-byte transport cannot fit the reply until the peer reads.
        let err = tokio::time::timeout(std::time::Duration::from_millis(200), task)
            .await
            .expect("deadline includes version write")
            .unwrap()
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
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

    struct BeginProbe(Arc<std::sync::atomic::AtomicUsize>);
    impl QueryEngine for BeginProbe {
        fn begin(&mut self, _: Option<&str>) -> Result<(), crate::session::EngineError> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
            Ok(())
        }
    }

    struct SlowLogon(BeginProbe);
    impl QueryEngine for SlowLogon {
        fn set_access(
            &mut self,
            access: crate::auth::AccessContext,
        ) -> Result<(), crate::session::EngineError> {
            if matches!(access, crate::auth::AccessContext::Unrestricted) {
                std::thread::sleep(std::time::Duration::from_millis(60));
            }
            Ok(())
        }
        fn begin(&mut self, db: Option<&str>) -> Result<(), crate::session::EngineError> {
            self.0.begin(db)
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
    async fn expired_authentication_after_blocking_logon_never_executes_buffered_begin() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (server, mut client) = tokio::io::duplex(1024);
        let task = tokio::spawn(handle_connection_with_timeouts(
            server,
            SlowLogon(BeginProbe(calls.clone())),
            64,
            AuthMode::AllowAll,
            std::time::Duration::from_secs(1),
            std::time::Duration::from_millis(40),
        ));
        handshake(&mut client).await;
        let mut pipeline = Vec::new();
        for tag in [0x01, 0x6a, 0x11] {
            pipeline.extend_from_slice(&encode_message(&[0xb1, tag, 0xa0]));
        }
        client.write_all(&pipeline).await.unwrap();
        read_success(&mut client).await;
        authentication_timed_out(task).await;
        let mut bytes = Vec::new();
        client.read_to_end(&mut bytes).await.unwrap();
        assert!(
            bytes.is_empty(),
            "expired LOGON sends no SUCCESS or FAILURE"
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn trailing_payload_bytes_fail_before_calling_the_engine() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (server, mut client) = tokio::io::duplex(1024);
        let task = tokio::spawn(handle_connection(
            server,
            BeginProbe(calls.clone()),
            64,
            AuthMode::AllowAll,
        ));
        handshake(&mut client).await;
        for tag in [0x01, 0x6a] {
            send_request(&mut client, tag, vec![PackValue::Map(vec![])]).await;
            read_success(&mut client).await;
        }
        client
            .write_all(&encode_message(&[0xb1, 0x11, 0xa0, 0xe0]))
            .await
            .unwrap();
        let mut failure = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            client.read_to_end(&mut failure),
        )
        .await
        .expect("close on trailing bytes")
        .unwrap();
        assert_eq!(decode_failure(&failure).1, "malformed message");
        task.await.unwrap().unwrap();
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
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

    #[tokio::test]
    async fn coalesced_messages_each_have_their_own_payload_limit() {
        let (server, mut client) = tokio::io::duplex(1024);
        let task = tokio::spawn(handle_connection(server, StubEngine, 3, AuthMode::AllowAll));
        handshake(&mut client).await;
        let mut pipeline = Vec::new();
        for body in [&[0xb1, 0x01, 0xa0][..], &[0xb1, 0x6a, 0xa0], &[0xb0, 0x02]] {
            pipeline.extend_from_slice(&encode_message(body));
        }
        client.write_all(&pipeline).await.unwrap();
        read_success(&mut client).await;
        read_success(&mut client).await;
        let mut rest = Vec::new();
        client.read_to_end(&mut rest).await.unwrap();
        assert!(rest.is_empty());
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn fragmented_second_message_keeps_its_accumulated_payload_size() {
        let (server, mut client) = tokio::io::duplex(1024);
        let task = tokio::spawn(handle_connection(
            server,
            StubEngine,
            32,
            AuthMode::AllowAll,
        ));
        handshake(&mut client).await;
        let mut bytes = encode_message(&[0xb1, 0x01, 0xa0]);
        bytes.extend_from_slice(&[0, 20]);
        bytes.extend_from_slice(&[0; 20]);
        client.write_all(&bytes).await.unwrap();
        read_success(&mut client).await;
        // This header announces a total payload of 33 bytes, without a body
        // or terminator. The first message must not erase the second's size.
        client.write_all(&[0]).await.unwrap();
        client.write_all(&[13]).await.unwrap();
        let mut failure = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            client.read_to_end(&mut failure),
        )
        .await
        .expect("close on oversized announcement")
        .unwrap();
        assert!(decode_failure(&failure).1.contains("32 bytes"));
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn oversized_chunk_header_is_rejected_without_waiting_for_body() {
        let (server, mut client) = tokio::io::duplex(1024);
        let task = tokio::spawn(handle_connection(
            server,
            StubEngine,
            64,
            AuthMode::AllowAll,
        ));
        handshake(&mut client).await;
        client.write_all(&[0xff]).await.unwrap();
        client.write_all(&[0xff]).await.unwrap();
        let mut failure = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            client.read_to_end(&mut failure),
        )
        .await
        .expect("close on oversized header")
        .unwrap();
        assert!(decode_failure(&failure).1.contains("64 bytes"));
        task.await.unwrap().unwrap();
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
                client.write_all(&[0, 65]).await.unwrap();
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
        use crate::auth::{AccessContext, GraphAccess};
        let policy = protected_test_policy();
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
    #[tokio::test]
    async fn decoded_overbudget_begin_is_fatal_before_engine_and_releases_permit() {
        for malformed in [false, true] {
            let (server, mut client) = tokio::io::duplex(1024);
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let permits = Arc::new(Semaphore::new(1));
            let permit = permits.clone().acquire_owned().await.unwrap();
            let probe = calls.clone();
            let task = tokio::spawn(async move {
                let _permit = permit;
                handle_connection_with_limits_and_timeouts(
                    server,
                    BeginProbe(probe),
                    4096,
                    512,
                    AuthMode::AllowAll,
                    DEFAULT_HANDSHAKE_TIMEOUT,
                    DEFAULT_AUTHENTICATION_TIMEOUT,
                )
                .await
            });
            handshake(&mut client).await;
            for tag in [0x01, 0x6a] {
                send_request(&mut client, tag, vec![PackValue::Map(vec![])]).await;
                read_success(&mut client).await;
            }
            if malformed {
                client
                    .write_all(&encode_message(&[0xb1, 0x11, 0xda, 0xff, 0xff, 0xff, 0xff]))
                    .await
                    .unwrap();
            } else {
                send_request(
                    &mut client,
                    0x11,
                    vec![PackValue::Map(vec![(
                        "metadata".into(),
                        PackValue::List(vec![PackValue::Null; 100]),
                    )])],
                )
                .await;
            }
            let mut failure = Vec::new();
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                client.read_to_end(&mut failure),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(
                decode_failure(&failure).0,
                "Neo.ClientError.Request.Invalid"
            );
            task.await.unwrap().unwrap();
            assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
            assert!(permits.try_acquire().is_ok());
        }
    }

    #[tokio::test]
    async fn small_encoded_cap_is_independent_of_decoded_storage_cap() {
        let (server, mut client) = tokio::io::duplex(1024);
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let probe = calls.clone();
        let task = tokio::spawn(handle_connection_with_limits_and_timeouts(
            server,
            BeginProbe(probe),
            32,
            512,
            AuthMode::AllowAll,
            DEFAULT_HANDSHAKE_TIMEOUT,
            DEFAULT_AUTHENTICATION_TIMEOUT,
        ));
        handshake(&mut client).await;
        for tag in [0x01, 0x6a, 0x11] {
            send_request(&mut client, tag, vec![PackValue::Map(vec![])]).await;
            read_success(&mut client).await;
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        client.write_all(&[0, 33]).await.unwrap();
        let mut failure = Vec::new();
        client.read_to_end(&mut failure).await.unwrap();
        assert!(decode_failure(&failure).1.contains("32 bytes"));
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn decoded_overbudget_cleanup_precedes_a_blocked_failure_write() {
        let (server, mut client) = tokio::io::duplex(32);
        let aborted = Arc::new(tokio::sync::Notify::new());
        let task = tokio::spawn(handle_connection_with_limits_and_timeouts(
            server,
            CleanupEngine(aborted.clone()),
            4096,
            512,
            AuthMode::AllowAll,
            DEFAULT_HANDSHAKE_TIMEOUT,
            DEFAULT_AUTHENTICATION_TIMEOUT,
        ));
        login_and_begin(&mut client).await;
        let mut payload = vec![0xd4, 100];
        payload.extend_from_slice(&[0xc0; 100]);
        client.write_all(&encode_message(&payload)).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), aborted.notified())
            .await
            .unwrap();
        assert!(!task.is_finished());
        let mut failure = Vec::new();
        client.read_to_end(&mut failure).await.unwrap();
        assert_eq!(
            decode_failure(&failure).0,
            "Neo.ClientError.Request.Invalid"
        );
        task.await.unwrap().unwrap();
    }
    #[tokio::test]
    async fn socket_io_error_aborts_before_the_session_is_dropped() {
        let aborted = Arc::new(tokio::sync::Notify::new());
        let mut session = Session::new(CleanupEngine(aborted.clone()), AuthMode::AllowAll);
        for tag in [0x01, 0x6a, 0x11] {
            let request = parse_request(PackValue::Struct {
                tag,
                fields: vec![PackValue::Map(vec![])],
            })
            .unwrap();
            session.handle(request);
        }
        assert_eq!(session.state(), State::TxReady);
        let error = io_with_deadline(&mut session, None, async {
            Err::<(), _>(std::io::Error::from(std::io::ErrorKind::ConnectionReset))
        })
        .await
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset);
        tokio::time::timeout(std::time::Duration::from_millis(100), aborted.notified())
            .await
            .unwrap();
        assert_eq!(session.state(), State::Defunct);
    }
    fn short_io_connection<E: QueryEngine + Send + 'static>(
        server: tokio::io::DuplexStream,
        engine: E,
    ) -> tokio::task::JoinHandle<std::io::Result<()>> {
        tokio::spawn(handle_connection_with_connection_timeouts(
            server,
            engine,
            ConnectionLimits {
                io_timeout: std::time::Duration::from_millis(80),
                ..ConnectionLimits::default()
            },
            AuthMode::AllowAll,
            DEFAULT_HANDSHAKE_TIMEOUT,
            DEFAULT_AUTHENTICATION_TIMEOUT,
        ))
    }

    #[tokio::test]
    async fn authenticated_idle_noops_and_partial_requests_expire_and_abort_pending_transaction() {
        for mode in 0..3 {
            let (server, mut client) = tokio::io::duplex(1024);
            let aborted = Arc::new(tokio::sync::Notify::new());
            let task = short_io_connection(server, CleanupEngine(aborted.clone()));
            login_and_begin(&mut client).await;
            let drip = tokio::spawn(async move {
                if mode != 0 {
                    let bytes = if mode == 1 { vec![0, 0] } else { vec![0] };
                    loop {
                        if client.write_all(&bytes).await.is_err() {
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(15)).await;
                    }
                } else {
                    let mut bytes = Vec::new();
                    client.read_to_end(&mut bytes).await.unwrap();
                }
            });
            let error = tokio::time::timeout(std::time::Duration::from_millis(400), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
            tokio::time::timeout(std::time::Duration::from_millis(100), aborted.notified())
                .await
                .unwrap();
            drip.abort();
        }
    }

    #[tokio::test]
    async fn complete_authenticated_requests_renew_the_io_budget() {
        let (server, mut client) = tokio::io::duplex(1024);
        let task = short_io_connection(server, StubEngine);
        handshake(&mut client).await;
        for tag in [0x01, 0x6a] {
            send_request(&mut client, tag, vec![PackValue::Map(vec![])]).await;
            read_success(&mut client).await;
        }
        for _ in 0..5 {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            send_request(&mut client, 0x54, vec![]).await;
            read_success(&mut client).await;
        }
        send_request(&mut client, 0x02, vec![]).await;
        task.await.unwrap().unwrap();
    }

    struct SlowQuery;
    impl QueryEngine for SlowQuery {
        fn begin(&mut self, _: Option<&str>) -> Result<(), crate::session::EngineError> {
            Ok(())
        }
        fn commit(&mut self) -> Result<Option<String>, crate::session::EngineError> {
            Ok(None)
        }
        fn abort(&mut self) -> Result<(), crate::session::EngineError> {
            Ok(())
        }
        fn run(
            &mut self,
            db: Option<&str>,
            query: &str,
            params: &PackValue,
        ) -> Result<crate::session::QueryResult, crate::session::EngineError> {
            std::thread::sleep(std::time::Duration::from_millis(160));
            StubEngine.run(db, query, params)
        }
    }

    #[tokio::test]
    async fn blocking_query_is_excluded_from_authenticated_request_io_budget() {
        let (server, mut client) = tokio::io::duplex(1024);
        let task = short_io_connection(server, SlowQuery);
        handshake(&mut client).await;
        for tag in [0x01, 0x6a] {
            send_request(&mut client, tag, vec![PackValue::Map(vec![])]).await;
            read_success(&mut client).await;
        }
        send_request(
            &mut client,
            0x10,
            vec![
                PackValue::String("RETURN 1".into()),
                PackValue::Map(vec![]),
                PackValue::Map(vec![]),
            ],
        )
        .await;
        read_success(&mut client).await;
        send_request(&mut client, 0x0f, vec![]).await;
        read_success(&mut client).await;
        send_request(&mut client, 0x02, vec![]).await;
        task.await.unwrap().unwrap();
    }
    struct ManyRows(Arc<tokio::sync::Notify>);
    impl QueryEngine for ManyRows {
        fn begin(&mut self, _: Option<&str>) -> Result<(), crate::session::EngineError> {
            Ok(())
        }
        fn commit(&mut self) -> Result<Option<String>, crate::session::EngineError> {
            Ok(None)
        }
        fn abort(&mut self) -> Result<(), crate::session::EngineError> {
            self.0.notify_one();
            Ok(())
        }
        fn run(
            &mut self,
            _: Option<&str>,
            _: &str,
            _: &PackValue,
        ) -> Result<crate::session::QueryResult, crate::session::EngineError> {
            Ok(crate::session::QueryResult {
                fields: vec!["value".into()],
                rows: vec![vec![PackValue::String("x".repeat(100))]; 10],
            })
        }
    }

    #[tokio::test]
    async fn all_response_frames_share_one_absolute_write_budget() {
        let (server, mut client) = tokio::io::duplex(32);
        let aborted = Arc::new(tokio::sync::Notify::new());
        let task = short_io_connection(server, ManyRows(aborted.clone()));
        login_and_begin(&mut client).await;
        send_request(
            &mut client,
            0x10,
            vec![
                PackValue::String("RETURN 1".into()),
                PackValue::Map(vec![]),
                PackValue::Map(vec![]),
            ],
        )
        .await;
        read_success(&mut client).await;
        send_request(
            &mut client,
            0x3f,
            vec![PackValue::Map(vec![("n".into(), PackValue::Int(-1))])],
        )
        .await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let size = client.read_u16().await.unwrap() as usize;
        let mut record = vec![0; size];
        client.read_exact(&mut record).await.unwrap();
        assert_eq!(client.read_u16().await.unwrap(), 0);
        assert!(matches!(
            unpack(&mut record.as_slice()).unwrap(),
            PackValue::Struct { tag: 0x71, .. }
        ));
        // The first frame consumed 50ms; the second gets only the remaining 30ms,
        // not a fresh 80ms. Keep the peer open without draining the next frame.
        let error = tokio::time::timeout(std::time::Duration::from_millis(60), task)
            .await
            .expect("write budget must not renew between response frames")
            .unwrap()
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        tokio::time::timeout(std::time::Duration::from_millis(100), aborted.notified())
            .await
            .unwrap();
    }
}
