#[cfg(feature = "arena")]
use campfire_server::Campfire;
use campfire_server::cli::Options;
#[cfg(feature = "arena")]
use campfire_server::dashboard::{Dashboard, DashboardOptions};
use std::error::Error;
#[cfg(feature = "arena")]
use std::{net::SocketAddr, time::Duration};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print!("{}", campfire_server::cli::help());
        return Ok(());
    }
    let options = Options::parse(args, |key| std::env::var(key).ok())?;
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .json()
        .init();
    #[cfg(not(feature = "arena"))]
    {
        let _ = options;
        Err("No game modules compiled. Build with --features arena, or add a trusted module in the server composition root.".into())
    }
    #[cfg(feature = "arena")]
    {
        let admin_options = DashboardOptions::from_env()?;
        let listener = tokio::net::TcpListener::bind(options.bind).await?;
        let address = listener.local_addr()?;
        let runtime = Campfire::new(options.config)?;
        for game_id in &options.games {
            if let Err(error) = runtime.register_game(game_id.clone(), campfire_server::ArenaModule)
            {
                runtime.shutdown().await;
                return Err(error.into());
            }
            tracing::info!(game_id, "game module registered");
        }
        let mut dashboard = match admin_options {
            Some(options) => match Dashboard::start(runtime.observer(), options) {
                Ok(dashboard) => Some(dashboard),
                Err(error) => {
                    runtime.shutdown().await;
                    return Err(error.into());
                }
            },
            None => None,
        };
        let app = dashboard.as_ref().map_or_else(
            || runtime.router(),
            |dashboard| runtime.router().merge(dashboard.router()),
        );
        tracing::info!(bind = %address, "Campfire listening");
        let metrics_runtime = runtime.clone();
        let metrics_task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(10));
            loop {
                interval.tick().await;
                tracing::info!(metrics = ?metrics_runtime.metrics(), "runtime metrics");
            }
        });
        let shutdown_runtime = runtime.clone();
        let result = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            shutdown_runtime.shutdown().await;
        })
        .await;
        metrics_task.abort();
        let _ = metrics_task.await;
        if let Some(dashboard) = &mut dashboard {
            dashboard.shutdown().await;
        }
        runtime.shutdown().await;
        result?;
        Ok(())
    }
}

#[cfg(feature = "arena")]
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    tracing::info!("graceful shutdown requested");
}
