use arrow_mc::server::{MINECRAFT_VERSION, PROTOCOL_VERSION, Server, ServerConfig};
use std::{env, io, net::IpAddr, process::ExitCode, time::Duration};
use tokio::sync::watch;

fn main() -> ExitCode {
    match start() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Arrow MC: {error}");
            ExitCode::FAILURE
        }
    }
}

fn start() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = ServerConfig::default();
    let mut workers = std::thread::available_parallelism().map_or(1, |count| count.get().min(2));
    let mut args = env::args().skip(1);
    while let Some(option) = args.next() {
        if option == "--help" || option == "-h" {
            println!(
                "Arrow MC {MINECRAFT_VERSION} (protocol {PROTOCOL_VERSION})\nUsage: arrow-mc [--bind IP] [--port PORT] [--description TEXT]\n                [--max-players N] [--max-connections N] [--timeout-seconds N]\n                [--connection-bytes N] [--io-workers N]\nServes Java status/ping. Login and gameplay are not implemented yet."
            );
            return Ok(());
        }
        let value = args.next().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("missing value for {option}"),
            )
        })?;
        match option.as_str() {
            "--bind" => config.bind.set_ip(value.parse::<IpAddr>()?),
            "--port" => config.bind.set_port(value.parse()?),
            "--description" => config.description = value,
            "--max-players" => config.max_players = value.parse()?,
            "--max-connections" => config.max_connections = value.parse()?,
            "--timeout-seconds" => config.connection_timeout = Duration::from_secs(value.parse()?),
            "--connection-bytes" => config.max_connection_bytes = value.parse()?,
            "--io-workers" => workers = value.parse()?,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unknown option: {option}"),
                )
                .into());
            }
        }
    }
    if workers == 0 {
        return Err(
            io::Error::new(io::ErrorKind::InvalidInput, "I/O workers must be nonzero").into(),
        );
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let server = Server::bind(config).await?;
        println!("Arrow MC listening on {} — {MINECRAFT_VERSION}, protocol {PROTOCOL_VERSION}; status/ping available, login unavailable", server.local_addr()?);
        let (stop, receiver) = watch::channel(false);
        let signal = tokio::spawn(async move { shutdown_signal().await; let _ = stop.send(true); });
        let result = server.run(receiver).await;
        signal.abort();
        let _ = signal.await;
        result
    })?;
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
