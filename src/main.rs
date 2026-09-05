use arrow_mc::{
    runtime::{CpuPool, CpuPoolConfig},
    server::{
        LoginServices, MINECRAFT_VERSION, PROTOCOL_VERSION, Server, ServerConfig,
        access::LoginAccess,
        auth::{AuthClient, AuthLimits},
        configuration_data::{
            ConfigurationSnapshot, ExpectedReference, LoadLimits, PackFingerprint,
            REFERENCE_VERSION, parse_sha256,
        },
        crypto::ServerKey,
    },
};
use std::{env, io, net::IpAddr, path::PathBuf, process::ExitCode, sync::Arc, time::Duration};
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
    let mut description_explicit = false;
    let mut workers = std::thread::available_parallelism().map_or(1, |count| count.get().min(2));
    let mut snapshot_path = None;
    let mut manifest_hash = None;
    let mut cpu_workers = None;
    let mut cpu_jobs = 64;
    let mut cpu_bytes = 128 * 1024 * 1024;
    let mut max_login_connections = 8;
    let mut online_mode = true;
    let mut prevent_proxy_connections = false;
    let mut accepts_transfers = false;
    let mut compression_threshold = 256;
    let mut login_option_seen = false;
    let mut args = env::args().skip(1);
    while let Some(option) = args.next() {
        if option == "--help" || option == "-h" {
            println!(
                "Arrow MC {MINECRAFT_VERSION} (protocol {PROTOCOL_VERSION})\nUsage: arrow-mc [--bind IP] [--port PORT] [--description TEXT]\n                [--max-players N] [--max-connections N] [--timeout-seconds N]\n                [--connection-bytes N] [--io-workers N]\n                [--configuration-snapshot PATH --configuration-manifest-sha256 HASH]\n                [--online-mode true|false] [--prevent-proxy-connections true|false]\n                [--accepts-transfers true|false] [--compression-threshold N]\n                [--max-login-connections N] [--cpu-workers N] [--cpu-jobs N] [--cpu-bytes N]\nServes Java status/ping. A verified snapshot enables login and configuration.\nOnline authentication is the default. Configuration waits for world preparation; gameplay is unavailable."
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
            "--description" => {
                config.description = value;
                description_explicit = true;
            }
            "--max-players" => config.max_players = value.parse()?,
            "--max-connections" => config.max_connections = value.parse()?,
            "--timeout-seconds" => config.connection_timeout = Duration::from_secs(value.parse()?),
            "--connection-bytes" => config.max_connection_bytes = value.parse()?,
            "--io-workers" => workers = value.parse()?,
            "--configuration-snapshot" => snapshot_path = Some(PathBuf::from(value)),
            "--configuration-manifest-sha256" => manifest_hash = Some(parse_sha256(&value)?),
            "--online-mode" => {
                online_mode = value.parse()?;
                login_option_seen = true;
            }
            "--prevent-proxy-connections" => {
                prevent_proxy_connections = value.parse()?;
                login_option_seen = true;
            }
            "--accepts-transfers" => {
                accepts_transfers = value.parse()?;
                login_option_seen = true;
            }
            "--compression-threshold" => {
                compression_threshold = value.parse()?;
                login_option_seen = true;
            }
            "--max-login-connections" => {
                max_login_connections = value.parse()?;
                login_option_seen = true;
            }
            "--cpu-workers" => {
                cpu_workers = Some(value.parse::<usize>()?);
                login_option_seen = true;
            }
            "--cpu-jobs" => {
                cpu_jobs = value.parse()?;
                login_option_seen = true;
            }
            "--cpu-bytes" => {
                cpu_bytes = value.parse()?;
                login_option_seen = true;
            }
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
    if snapshot_path.is_some() != manifest_hash.is_some()
        || login_option_seen && snapshot_path.is_none()
    {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "login settings require both a configuration snapshot and its separately trusted manifest SHA-256").into());
    }
    let (stop, receiver) = watch::channel(false);
    let services = if let Some(path) = snapshot_path {
        if max_login_connections == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "login connection limit must be nonzero",
            )
            .into());
        }
        // These values are pinned from the verified reference, not read from
        // the candidate snapshot's self-reported provenance.
        let jar_hash =
            parse_sha256("18d6ad2986227ea55eb18f8ee6929999a4c48c0bbd623c36af3d2f64d3180e4a")?;
        let packs = [PackFingerprint {
            id: "vanilla".into(),
            version: REFERENCE_VERSION.into(),
            sha256: jar_hash,
        }];
        let snapshot = ConfigurationSnapshot::load(
            &path,
            &ExpectedReference {
                expected_manifest_sha256: manifest_hash.unwrap(),
                minecraft_version: REFERENCE_VERSION,
                protocol: PROTOCOL_VERSION,
                source_jar_sha256: jar_hash,
                source_jar_bytes: 26_649_663,
                selected_packs: &packs,
            },
            LoadLimits::default(),
        )?;
        let cpu_workers = cpu_workers.unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map_or(1, |count| count.get().saturating_sub(workers).max(1))
        });
        let cpu = CpuPool::new(CpuPoolConfig {
            workers: cpu_workers,
            max_jobs: cpu_jobs,
            buffer_bytes: cpu_bytes,
        })?;
        // Resource loading and key generation happen before accepting sockets.
        Some(LoginServices {
            key: Arc::new(ServerKey::generate()?),
            auth: Arc::new(AuthClient::new(AuthLimits::default())?),
            cpu: Arc::new(cpu),
            snapshot: Arc::new(snapshot),
            access: Arc::new(LoginAccess::new(config.max_players as usize)),
            compression_threshold,
            online_mode,
            prevent_proxy_connections,
            accepts_transfers,
            max_login_connections,
            shutdown: receiver.clone(),
        })
    } else {
        None
    };
    if services.is_some() && !description_explicit {
        config.description = "Arrow MC — preparing world".into();
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let login_enabled = services.is_some();
        let server = if let Some(services) = services { Server::bind_with_login(config, services).await? } else { Server::bind(config).await? };
        let mode = if login_enabled { "login/configuration available; waiting for world preparation" } else { "login unavailable; no configuration snapshot configured" };
        println!("Arrow MC listening on {} — {MINECRAFT_VERSION}, protocol {PROTOCOL_VERSION}; status/ping available, {mode}", server.local_addr()?);
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
