use std::{io::BufRead, time::Duration};

use anyhow::{bail, Context};
use clap::{Parser, Subcommand};
use notesync_server::{api, db, gc, users, AppState, Config};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "notesync-server",
    version,
    about = "Self-hosted sync server for Obsidian vaults"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the server.
    Serve,
    /// Manage user accounts.
    User {
        #[command(subcommand)]
        command: UserCommand,
    },
    /// Inspect vaults.
    Vault {
        #[command(subcommand)]
        command: VaultCommand,
    },
    /// Remove old file versions and deleted vaults now (also runs daily).
    Gc,
}

#[derive(Subcommand)]
enum UserCommand {
    /// Create a user.
    Add {
        username: String,
        /// Give the user admin rights.
        #[arg(long)]
        admin: bool,
        /// Read the password from standard input instead of prompting.
        #[arg(long)]
        password_stdin: bool,
    },
    /// Set a new password and sign the user out everywhere.
    Passwd {
        username: String,
        #[arg(long)]
        password_stdin: bool,
    },
    /// Block a user from signing in and sign them out everywhere.
    Disable { username: String },
    /// Allow a disabled user to sign in again.
    Enable { username: String },
    /// Give a user admin rights.
    MakeAdmin { username: String },
    /// Clear a sign-in lockout after repeated wrong passwords.
    Unlock { username: String },
    /// List all users.
    List,
}

#[derive(Subcommand)]
enum VaultCommand {
    /// List a user's vaults.
    List {
        #[arg(long)]
        user: String,
        /// Show deleted vaults that can still be restored.
        #[arg(long)]
        deleted: bool,
    },
    /// Restore a deleted vault (until cleanup removes it after the history period).
    Restore {
        #[arg(long)]
        user: String,
        /// The vault ID, from `vault list --deleted`.
        id: i64,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("NOTESYNC_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let config = Config::from_env()?;
    let state = AppState::new(config).await?;

    match cli.command {
        Command::Serve => serve(state).await,
        Command::User { command } => user_command(&state, command).await,
        Command::Vault { command } => vault_command(&state, command).await,
        Command::Gc => {
            let report = gc::run(&state).await?;
            println!(
                "Removed {} stored file versions and {} deleted vaults.",
                report.blobs_removed, report.vaults_removed
            );
            Ok(())
        }
    }
}

async fn serve(state: AppState) -> anyhow::Result<()> {
    let addr = state.config.bind_addr;

    let gc_state = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(24 * 60 * 60));
        loop {
            interval.tick().await;
            match gc::run(&gc_state).await {
                Ok(report) => tracing::info!(?report, "cleanup finished"),
                Err(e) => tracing::error!("cleanup failed: {e:#}"),
            }
        }
    });

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!("listening on http://{addr}");
    axum::serve(listener, api::router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutting down");
}

async fn user_command(state: &AppState, command: UserCommand) -> anyhow::Result<()> {
    let pool = &state.pool;
    match command {
        UserCommand::Add {
            username,
            admin,
            password_stdin,
        } => {
            let password = read_password(password_stdin)?;
            users::create_user(pool, &username, &password, admin).await?;
            println!("Created user {username}.");
        }
        UserCommand::Passwd {
            username,
            password_stdin,
        } => {
            let password = read_password(password_stdin)?;
            users::set_password(pool, &username, &password).await?;
            println!("Changed the password for {username} and signed them out everywhere.");
        }
        UserCommand::Disable { username } => {
            users::set_disabled(pool, &username, true).await?;
            println!("Disabled {username} and signed them out everywhere.");
        }
        UserCommand::Enable { username } => {
            users::set_disabled(pool, &username, false).await?;
            println!("Enabled {username}.");
        }
        UserCommand::MakeAdmin { username } => {
            users::set_admin(pool, &username, true).await?;
            println!("{username} is now an admin.");
        }
        UserCommand::Unlock { username } => {
            users::unlock(pool, &username).await?;
            println!("Cleared the sign-in lockout for {username}.");
        }
        UserCommand::List => {
            println!("ID     USERNAME                 ADMIN  DISABLED  DEVICES  LOCKED");
            let now = db::now();
            for user in users::list(pool).await? {
                let locked = if user.locked_until > now {
                    format!("{} min", (user.locked_until - now + 59) / 60)
                } else {
                    "no".to_string()
                };
                println!(
                    "{:<6} {:<24} {:<6} {:<9} {:<8} {}",
                    user.id,
                    user.username,
                    yes_no(user.is_admin),
                    yes_no(user.disabled),
                    user.devices,
                    locked
                );
            }
        }
    }
    Ok(())
}

async fn vault_command(state: &AppState, command: VaultCommand) -> anyhow::Result<()> {
    match command {
        VaultCommand::List { user, deleted } => {
            let vaults = users::vaults(&state.pool, &user, deleted).await?;
            if deleted {
                let days = state.config.history_days as i64;
                println!("ID     NAME                             CHANGES  REMOVED IN");
                for v in vaults {
                    let left = (v.deleted_at.unwrap_or(0) + days * 86_400 - db::now()) / 86_400;
                    println!(
                        "{:<6} {:<32} {:<8} {} days",
                        v.id,
                        v.name,
                        v.head_seq,
                        left.max(0)
                    );
                }
            } else {
                println!("ID     NAME                             CHANGES");
                for v in vaults {
                    println!("{:<6} {:<32} {}", v.id, v.name, v.head_seq);
                }
            }
        }
        VaultCommand::Restore { user, id } => {
            users::restore_vault(&state.pool, &user, id).await?;
            println!("Restored vault {id} for {user}.");
        }
    }
    Ok(())
}

fn yes_no(flag: i64) -> &'static str {
    if flag != 0 {
        "yes"
    } else {
        "no"
    }
}

fn read_password(from_stdin: bool) -> anyhow::Result<String> {
    if from_stdin {
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        return Ok(line.trim_end_matches(['\r', '\n']).to_string());
    }
    let first = rpassword::prompt_password("Password: ")?;
    let second = rpassword::prompt_password("Repeat password: ")?;
    if first != second {
        bail!("passwords don't match");
    }
    Ok(first)
}
