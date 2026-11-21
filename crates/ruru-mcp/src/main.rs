use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use ruru_mcp::config::Config;
use ruru_mcp::secrets::Secrets;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

/// The whiteboard MCP service: HTTP MCP endpoint + admin panel + capture pipeline.
#[derive(Debug, Parser)]
#[command(name = "ruru-mcp", version, about)]
struct Args {
    /// Path to the TOML config.
    #[arg(short, long, default_value = "/etc/ruru/mcp.toml", global = true)]
    config: PathBuf,

    /// Override the secrets file path (otherwise taken from config, else the default).
    #[arg(long, global = true)]
    secrets: Option<PathBuf>,

    /// Log filter, e.g. `info`, `debug`, `ruru_mcp=debug,info`.
    #[arg(long, env = "RURU_LOG", default_value = "info", global = true)]
    log: String,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the server (default).
    Serve,
    /// Create the secrets file (fresh cookie key) if it does not exist.
    Init,
    /// Set or replace the admin password (prompts; argon2id).
    SetPassword,
    /// Mint or rotate the 256-bit bearer key. Prints the key once; stores only its hash.
    MintKey,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_new(&args.log).context("parsing --log filter")?)
        .init();

    match args.command.as_ref().unwrap_or(&Command::Serve) {
        Command::Serve => {
            let config = Config::load(&args.config)?;
            ruru_mcp::serve(config).await
        }
        Command::Init => {
            let path = secrets_path(&args)?;
            let secrets = Secrets::load_or_init(&path)?;
            secrets.save(&path)?;
            println!("secrets ready at {}", path.display());
            Ok(())
        }
        Command::SetPassword => set_password(&args),
        Command::MintKey => mint_key(&args),
    }
}

/// Where the secrets file lives: an explicit `--secrets` wins, otherwise the config's
/// path, otherwise the built-in default. Bootstrapping must work before a config exists.
fn secrets_path(args: &Args) -> Result<PathBuf> {
    if let Some(path) = &args.secrets {
        return Ok(path.clone());
    }
    match Config::load(&args.config) {
        Ok(config) => Ok(config.secrets.path),
        Err(_) => Ok(PathBuf::from("/etc/ruru/mcp-secrets.toml")),
    }
}

fn set_password(args: &Args) -> Result<()> {
    let path = secrets_path(args)?;
    let mut secrets = Secrets::load_or_init(&path)?;

    let password = read_new_password()?;
    if password.len() < 8 {
        bail!("password must be at least 8 characters");
    }

    secrets.set_password(&password)?;
    secrets.save(&path)?;
    println!("admin password set ({})", path.display());
    Ok(())
}

/// Interactively prompt (with confirmation) at a terminal; otherwise read a single line
/// from stdin, so the password can be provisioned by a script (systemd, ansible, `echo … |`).
fn read_new_password() -> Result<String> {
    use std::io::IsTerminal;

    if std::io::stdin().is_terminal() {
        let password =
            rpassword::prompt_password("New admin password: ").context("reading password")?;
        let confirm =
            rpassword::prompt_password("Confirm password: ").context("reading password")?;
        if password != confirm {
            bail!("passwords did not match");
        }
        Ok(password)
    } else {
        use std::io::BufRead;
        let mut line = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .context("reading password from stdin")?;
        let password = line.trim_end_matches(['\r', '\n']).to_string();
        if password.is_empty() {
            bail!("no password on stdin");
        }
        Ok(password)
    }
}

fn mint_key(args: &Args) -> Result<()> {
    let path = secrets_path(args)?;
    let mut secrets = Secrets::load_or_init(&path)?;

    let replacing = secrets.has_bearer();
    let token = secrets.mint_bearer();
    secrets.save(&path)?;

    if replacing {
        eprintln!("NOTE: the previous bearer key has been revoked.");
    }
    // Printed to stdout, once. Only the SHA-256 is persisted.
    println!();
    println!("  Bearer key — shown once, store it now:");
    println!();
    println!("      {token}");
    println!();
    println!("  Configure the MCP client with header:  Authorization: Bearer {token}");
    Ok(())
}
