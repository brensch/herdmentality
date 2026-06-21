mod app;
mod model;
mod repository;
mod service;

use std::env;
use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Result;
use herdcore_protocol::v1::herdcore_server::HerdcoreServer;
use repository::Repository;
use service::{start_external_bot, HerdcoreService};
use tonic::transport::Server;
use tonic_web::GrpcWebLayer;
use tower_http::cors::CorsLayer;

#[tokio::main]
async fn main() -> Result<()> {
    let address: SocketAddr = env::var("HERDCORE_LISTEN")
        .unwrap_or_else(|_| "127.0.0.1:55051".to_owned())
        .parse()?;
    let public_url =
        env::var("HERDCORE_PUBLIC_URL").unwrap_or_else(|_| format!("http://{address}"));
    let database_path = env::var("HERDCORE_DB").unwrap_or_else(|_| "target/herdcore.db".to_owned());
    let bot_provider_url = env::var("HERDCORE_BOT_PROVIDER_URL").ok();

    let repository = Repository::open(&database_path).await?;
    let app = app::App::load(repository).await?;
    if let Some(provider_url) = &bot_provider_url {
        for assignment in app.recoverable_bots().await {
            if let Err(error) = start_external_bot(provider_url, &public_url, &assignment).await {
                eprintln!(
                    "failed to restore external bot {}: {error}",
                    assignment.player_id
                );
            }
        }
    }
    let service = HerdcoreService::new(app, bot_provider_url, public_url);

    println!("Herdcore server listening on {address}");
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server = Server::builder()
        .accept_http1(true)
        .layer(CorsLayer::permissive())
        .layer(GrpcWebLayer::new())
        .add_service(HerdcoreServer::new(service))
        .serve_with_shutdown(address, async {
            let _ = shutdown_rx.await;
        });
    tokio::pin!(server);
    tokio::select! {
        result = &mut server => result?,
        signal = tokio::signal::ctrl_c() => {
            signal?;
            println!("Shutting down Herdcore server…");
            let _ = shutdown_tx.send(());
            match tokio::time::timeout(Duration::from_secs(1), &mut server).await {
                Ok(result) => result?,
                Err(_) => eprintln!("Closing remaining client streams"),
            }
        }
    }
    Ok(())
}
