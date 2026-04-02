mod serve;

use ostia_sandbox::SandboxExecutor;
use ostia_core::OstiaConfig;
use clap::{Parser, Subcommand};

use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "ostia", about = "OS-level CLI sandbox for AI agents")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Execute a command inside the sandbox
    Run {
        /// Path to the Ostia config file
        #[arg(long)]
        config: PathBuf,

        /// Profile name to use
        #[arg(long)]
        profile: String,

        /// Command and arguments to execute
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },

    /// Validate config and show profile details
    Check {
        /// Path to the Ostia config file
        #[arg(long)]
        config: PathBuf,

        /// Profile name to inspect
        #[arg(long)]
        profile: String,
    },

    /// Start the MCP server
    Serve {
        /// Path to the Ostia config file
        #[arg(long)]
        config: PathBuf,

        /// Transport protocol: stdio (default) or http
        #[arg(long, default_value = "stdio")]
        transport: String,

        /// Port for HTTP transport
        #[arg(long)]
        port: Option<u16>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        Commands::Run { config, profile, command } => {
            run_command(&config, &profile, &command)
        }
        Commands::Check { config, profile } => {
            check_command(&config, &profile)
        }
        Commands::Serve { config, transport, port } => {
            let rt = tokio::runtime::Runtime::new().unwrap();
            match rt.block_on(serve::run_serve(&config, &transport, port)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("error: {}", e);
                    ExitCode::from(1)
                }
            }
        }
    }
}

fn run_command(config_path: &PathBuf, profile_name: &str, command: &[String]) -> ExitCode {
    let config = match OstiaConfig::load(config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: failed to load config: {}", e);
            return ExitCode::from(1);
        }
    };

    let profile = match config.resolve_profile(profile_name) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: failed to resolve profile: {}", e);
            return ExitCode::from(1);
        }
    };

    let executor = match SandboxExecutor::from_profile(profile) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("error: failed to create sandbox: {}", e);
            return ExitCode::from(1);
        }
    };

    let full_command = command.join(" ");

    match executor.execute(&full_command) {
        Ok(result) if !result.allowed => {
            eprintln!(
                "denied: {}",
                result.reason.as_deref().unwrap_or("command not allowed")
            );
            ExitCode::from(1)
        }
        Ok(result) => {
            if !result.stdout.is_empty() {
                print!("{}", result.stdout);
            }
            if !result.stderr.is_empty() {
                eprint!("{}", result.stderr);
            }
            ExitCode::from(result.exit_code as u8)
        }
        Err(e) => {
            eprintln!("error: {}", e);
            ExitCode::from(1)
        }
    }
}

fn check_command(config_path: &PathBuf, profile_name: &str) -> ExitCode {
    let config = match OstiaConfig::load(config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: failed to load config: {}", e);
            return ExitCode::from(1);
        }
    };

    let profile = match config.resolve_profile(profile_name) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: failed to resolve profile: {}", e);
            return ExitCode::from(1);
        }
    };

    println!("Profile: {}", profile.name);
    println!();

    // Show binaries and their resolution status
    println!("Binaries:");
    let mut binaries: Vec<&String> = profile.binaries.iter().collect();
    binaries.sort();
    for binary in &binaries {
        match ostia_sandbox::resolve::which(binary) {
            Ok(path) => println!("  [found]     {} -> {}", binary, path.display()),
            Err(_) => println!("  [missing]   {}", binary),
        }
    }
    println!();

    // Show subcommand patterns
    if !profile.subcommand_allows.is_empty() {
        println!("Subcommand allow patterns:");
        for pattern in &profile.subcommand_allows {
            println!("  + {}", pattern);
        }
        println!();
    }

    if !profile.subcommand_denies.is_empty() {
        println!("Subcommand deny patterns:");
        for pattern in &profile.subcommand_denies {
            println!("  - {}", pattern);
        }
        println!();
    }

    // Show auth status if any checks are configured
    if !profile.auth_checks.is_empty() {
        println!("Auth:");
        let results = ostia_core::run_auth_checks(&profile.auth_checks);
        for result in &results {
            let status = if result.active { "active" } else { "inactive" };
            println!("  [{}] {}", status, result.service);
        }
        println!();
    }

    ExitCode::SUCCESS
}
