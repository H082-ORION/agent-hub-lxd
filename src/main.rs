mod capacity;
mod config;
mod enroll;
mod instances;
mod lxd;
mod protocol;
mod state;
mod tunnel;

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use crate::config::Config;
use crate::instances::Manager;
use crate::lxd::Lxd;

/// Exit code for a stop that a restart cannot fix (revoked token, protocol mismatch, bad configuration):
/// the systemd unit does not restart on it.
const EXIT_CONFIG: u8 = 78;

#[derive(Parser)]
#[command(
    version,
    about = "Creates opencode-hub cloud instances as LXD virtual machines"
)]
struct Cli {
    /// Configuration file
    #[arg(
        long,
        env = "AGENT_HUB_LXD_CONFIG",
        default_value = "/etc/agent-hub-lxd/config.toml",
        global = true
    )]
    config: PathBuf,
    /// Directory holding the enrollment (host token)
    #[arg(
        long,
        env = "AGENT_HUB_LXD_STATE",
        default_value = "/var/lib/agent-hub-lxd",
        global = true
    )]
    state_dir: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Enroll this LXD host with a hub; the one-time code is asked without echo, or read from stdin
    Join {
        /// Hub URL, e.g. https://hub.example.com
        url: String,
        /// Host name shown on the hub (default: `name` from the configuration, else the host name)
        #[arg(long)]
        name: Option<String>,
        /// Allow a plain HTTP hub URL on another machine (code and token in clear text)
        #[arg(long)]
        insecure: bool,
        /// Replace an existing enrollment
        #[arg(long)]
        force: bool,
    },
    /// Keep this host connected to its hub and serve its requests
    Run,
    /// Check the configuration and LXD, and show the enrollment and the host's capacity
    Status,
    /// Forget this host's enrollment (revoke it on the hub as well)
    Leave,
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "agent_hub_lxd=info".into()),
        )
        .with_target(false)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();
    match execute(Cli::parse()).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn execute(cli: Cli) -> Result<ExitCode> {
    let config = match Config::load(&cli.config) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("error: {error:#}");
            return Ok(ExitCode::from(EXIT_CONFIG));
        }
    };
    match cli.command {
        Command::Join {
            url,
            name,
            insecure,
            force,
        } => {
            if !force && state::load(&cli.state_dir)?.is_some() {
                bail!("this host is already enrolled; pass --force to replace the enrollment");
            }
            let code = enroll::read_code()?;
            let name = name.unwrap_or_else(|| config.host_name());
            let enrollment = enroll::enroll(&url, code, &name, insecure).await?;
            state::save(&cli.state_dir, &enrollment)?;
            println!(
                "Enrolled as {} ({}) with {}",
                enrollment.host.name, enrollment.host.id, enrollment.url
            );
            println!("Run `agent-hub-lxd run` (or start the agent-hub-lxd service) to connect.");
            Ok(ExitCode::SUCCESS)
        }
        Command::Run => {
            let Some(enrollment) = state::load(&cli.state_dir)? else {
                eprintln!("error: not enrolled; run `agent-hub-lxd join <hub url>` first");
                return Ok(ExitCode::from(EXIT_CONFIG));
            };
            let manager = manager(config, enrollment.url.clone())?;
            if let Err(error) = manager.check().await {
                eprintln!("error: {error}");
                return Ok(ExitCode::from(EXIT_CONFIG));
            }
            let fatal = tunnel::run(&enrollment, manager).await;
            tunnel::log_fatal(&fatal);
            Ok(ExitCode::from(EXIT_CONFIG))
        }
        Command::Status => {
            match state::load(&cli.state_dir)? {
                Some(enrollment) => {
                    println!(
                        "Enrolled as {} ({}) with {}",
                        enrollment.host.name, enrollment.host.id, enrollment.url
                    )
                }
                None => println!("Not enrolled."),
            }
            let manager = manager(config, String::new())?;
            println!("LXD {}", manager.lxd_version().await?);
            manager.check().await?;
            println!("Project, template and storage pool: OK");
            println!(
                "{}",
                serde_json::to_string_pretty(&manager.capacity().await?)?
            );
            for instance in manager.list().await? {
                println!(
                    "{} {} {:?}",
                    instance.instance, instance.status, instance.limits
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Leave => {
            if state::remove(&cli.state_dir)? {
                println!(
                    "Enrollment removed. Revoke the host on the hub too, so its token stops working."
                );
                return Ok(ExitCode::SUCCESS);
            }
            println!("Not enrolled.");
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn manager(config: Config, hub_url: String) -> Result<Arc<Manager>> {
    let socket = config.socket().context("finding LXD")?;
    let lxd = Lxd::new(socket, config.lxd.project.clone());
    Ok(Manager::new(lxd, config, hub_url))
}
