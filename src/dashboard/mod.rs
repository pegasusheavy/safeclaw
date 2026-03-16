pub mod auth;
pub mod authn;
pub mod binaries;
pub mod handlers;
pub mod messaging_webhook;
pub mod oauth;
pub mod routes;
pub mod skill_ext;
pub mod sse;

use std::sync::Arc;

use rusqlite::Connection;
use tokio::sync::{broadcast, Mutex};
use tracing::info;

use crate::agent::Agent;
use crate::config::{Config, TlsConfig};
use crate::error::{Result, SafeAgentError};
use crate::installer::BinaryInstaller;
use crate::messaging::MessagingManager;
use crate::trash::TrashManager;

pub async fn serve(
    config: Config,
    agent: Arc<Agent>,
    db: Arc<Mutex<Connection>>,
    db_read: Arc<Mutex<Connection>>,
    shutdown: broadcast::Receiver<()>,
    tls: Option<TlsConfig>,
    messaging: Arc<MessagingManager>,
    trash: Arc<TrashManager>,
    installer: BinaryInstaller,
) -> Result<()> {
    let app = routes::build(agent, config.clone(), db, db_read, messaging, trash, installer)?;

    // If ACME TLS is configured, serve over HTTPS using rustls-acme.
    // Otherwise fall back to plain HTTP on the dashboard_bind address.
    if let Some(ref tls_config) = tls {
        // Also start a plain-HTTP listener on the original dashboard_bind
        // address so local/internal traffic still works.
        let plain_app = app.clone();
        let plain_bind = config.dashboard_bind.clone();
        let plain_shutdown = shutdown.resubscribe();
        tokio::spawn(async move {
            if let Err(e) = serve_plain(plain_app, &plain_bind, plain_shutdown).await {
                tracing::warn!(err = %e, "plain HTTP listener failed (HTTPS is primary)");
            }
        });

        crate::acme::serve_https(tls_config, app, shutdown).await
    } else {
        serve_plain(app, &config.dashboard_bind, shutdown).await
    }
}

async fn serve_plain(
    app: axum::Router,
    bind: &str,
    mut shutdown: broadcast::Receiver<()>,
) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|e| SafeAgentError::Config(format!("failed to bind {bind}: {e}")))?;

    info!(bind = %bind, "dashboard listening (HTTP)");

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = shutdown.recv().await;
        })
        .await
        .map_err(|e| SafeAgentError::Config(format!("dashboard server error: {e}")))?;

    Ok(())
}
