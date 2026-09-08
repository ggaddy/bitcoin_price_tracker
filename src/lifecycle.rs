use axum::Router;
use hyper::server::conn::http1;
use hyper_util::{
    rt::{TokioIo, TokioTimer},
    service::TowerToHyperService,
};
use std::{future::Future, sync::Arc, time::Duration};
use tokio::{
    net::TcpListener,
    sync::{Semaphore, watch},
    task::JoinSet,
};

#[derive(Clone, Copy)]
pub(crate) struct ConnectionLimits {
    pub(crate) max_connections: usize,
    pub(crate) header_timeout: Duration,
    pub(crate) lifetime: Duration,
}

impl Default for ConnectionLimits {
    fn default() -> Self {
        Self {
            max_connections: 128,
            header_timeout: Duration::from_secs(5),
            lifetime: Duration::from_secs(25),
        }
    }
}

pub(crate) async fn serve_until(
    listener: TcpListener,
    app: Router,
    shutdown: impl Future<Output = ()>,
    grace: Duration,
    limits: ConnectionLimits,
) -> Result<(), String> {
    let slots = Arc::new(Semaphore::new(limits.max_connections));
    // Exec health probes connect over loopback. Remote clients cannot consume
    // these slots by supplying a forwarding header or saturating the origin.
    let local_slots = Arc::new(Semaphore::new(limits.max_connections.min(4)));
    let (stop, _) = watch::channel(false);
    let mut connections = JoinSet::new();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            biased;
            () = &mut shutdown => break,
            Some(result) = connections.join_next(), if !connections.is_empty() => {
                if let Err(error) = result {
                    tracing::warn!(%error, "connection task failed");
                }
            }
            accepted = listener.accept() => {
                let (stream, peer) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        tracing::warn!(%error, "failed to accept connection");
                        tokio::select! {
                            () = &mut shutdown => break,
                            () = tokio::time::sleep(Duration::from_secs(1)) => continue,
                        }
                    }
                };
                // Admission applies before headers are parsed. Never queue a
                // task or semaphore waiter for an overloaded connection.
                let pool = if peer.ip().is_loopback() { &local_slots } else { &slots };
                let Ok(permit) = pool.clone().try_acquire_owned() else {
                    drop(stream);
                    continue;
                };
                let service = TowerToHyperService::new(app.clone());
                let mut stopping = stop.subscribe();
                connections.spawn(async move {
                    let _permit = permit;
                    let mut builder = http1::Builder::new();
                    builder.timer(TokioTimer::new())
                        .header_read_timeout(limits.header_timeout)
                        .max_headers(64)
                        .max_buf_size(16 * 1024)
                        // One request per connection bounds idle keep-alive and
                        // prevents connection recycling from extending its life.
                        .keep_alive(false);
                    let connection = builder.serve_connection(TokioIo::new(stream), service);
                    tokio::pin!(connection);
                    let deadline = tokio::time::Instant::now() + limits.lifetime;
                    let result = tokio::select! {
                        result = tokio::time::timeout_at(deadline, &mut connection) => result,
                        _ = stopping.changed() => {
                            connection.as_mut().graceful_shutdown();
                            tokio::time::timeout_at(deadline, &mut connection).await
                        }
                    };
                    if !matches!(result, Ok(Ok(()))) {
                        tracing::debug!("connection ended before completing its response");
                    }
                });
            }
        }
    }
    drop(listener);
    tracing::info!(
        grace_seconds = grace.as_secs(),
        "shutdown requested; draining requests"
    );
    let _ = stop.send(true);
    if tokio::time::timeout(grace, async {
        while connections.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        connections.abort_all();
        while connections.join_next().await.is_some() {}
        return Err("graceful shutdown deadline exceeded".to_string());
    }
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
