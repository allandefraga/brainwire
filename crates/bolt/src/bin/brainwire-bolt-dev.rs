//! Development server: stub engine, no authentication. Never expose it.

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:7687".to_string());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    eprintln!("brainwire-bolt-dev listening on {addr} (stub engine, no authentication)");
    brainwire_bolt::server::serve(listener, || brainwire_bolt::session::StubEngine).await
}
