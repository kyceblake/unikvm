mod clipboard;
mod input;
mod peer;
mod service;

use std::env;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use tracing::Level;
use unikvm_config::{Config, LogLevel};

fn main() -> ExitCode {
    if let Err(error) = run() {
        eprintln!("unikvm-agent: {error:#}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn run() -> Result<()> {
    let mut args = env::args_os().skip(1);
    let first = args.next();
    let config = match first.as_deref().and_then(|arg| arg.to_str()) {
        Some("-h" | "--help") => {
            println!("Usage: unikvm-agent [--config PATH]");
            return Ok(());
        }
        Some("--config") => {
            let path = args.next().context("--config requires a path")?;
            if args.next().is_some() {
                bail!("usage: unikvm-agent [--config PATH]");
            }
            Config::load(Path::new(&path))
                .with_context(|| format!("configuration {}", Path::new(&path).display()))?
        }
        None if first.is_none() => Config::default(),
        _ => bail!("usage: unikvm-agent [--config PATH]"),
    };

    let level = match config.log_level {
        LogLevel::Error => Level::ERROR,
        LogLevel::Warn => Level::WARN,
        LogLevel::Info => Level::INFO,
        LogLevel::Debug => Level::DEBUG,
        LogLevel::Trace => Level::TRACE,
    };
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(level)
        .with_ansi(false)
        .try_init()
        .map_err(anyhow::Error::msg)?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(service::run(&config))
}
