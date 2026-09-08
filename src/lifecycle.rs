use axum::Router;
use std::{
    future::{Future, IntoFuture},
    time::Duration,
};
use tokio::{net::TcpListener, sync::oneshot};

pub(crate) async fn serve_until(
    listener: TcpListener,
    app: Router,
    shutdown: impl Future<Output = ()>,
    grace: Duration,
) -> Result<(), String> {
    let (send, receive) = oneshot::channel::<()>();
    let serve = axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = receive.await;
        })
        .into_future();
    tokio::pin!(serve);
    tokio::pin!(shutdown);
    tokio::select! {
        result = &mut serve => return result.map_err(|error| format!("HTTP server failed: {error}")),
        () = &mut shutdown => {}
    }
    tracing::info!(
        grace_seconds = grace.as_secs(),
        "shutdown requested; draining requests"
    );
    let _ = send.send(());
    tokio::time::timeout(grace, &mut serve)
        .await
        .map_err(|_| "graceful shutdown deadline exceeded".to_string())?
        .map_err(|error| format!("HTTP server shutdown failed: {error}"))?;
    tracing::info!("HTTP shutdown completed");
    Ok(())
}

pub(crate) async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! {
            result = tokio::signal::ctrl_c() => { result.expect("install SIGINT handler"); },
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c()
        .await
        .expect("install interrupt handler");
}
