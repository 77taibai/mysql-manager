mod client;
mod commands;
mod config;
mod error;
mod ini;
mod lifecycle;
mod process;
mod sql;
mod ui;

use clap::{Parser, Subcommand};
use error::Result;
use process::{Runner, SystemRunner};

#[derive(Parser)]
#[command(
    name = "mm",
    version,
    about = "Manage local MySQL instances: on-demand start/stop, database and user management",
    long_about = None
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Register, list or remove MySQL installations
    Instance {
        #[command(subcommand)]
        action: InstanceAction,
    },
    /// Start an instance in the background
    Start { name: String },
    /// Stop a running instance
    Stop { name: String },
    /// Show the status of one or all instances
    Status { name: Option<String> },
    /// Manage database accounts
    User {
        #[command(subcommand)]
        action: UserAction,
    },
    /// Manage databases
    Db {
        #[command(subcommand)]
        action: DbAction,
    },
    /// Bind a user to exactly one database, revoking all other privileges
    Bind {
        name: String,
        database: String,
        user: String,
        /// Account host, defaults to localhost
        #[arg(long)]
        host: Option<String>,
    },
    /// Remove a binding
    Unbind {
        name: String,
        user: String,
        #[arg(long)]
        host: Option<String>,
    },
    /// Run the one-shot test workflow
    Workflow { name: Option<String> },
    /// Set or update the root password for an instance
    Login {
        name: String,
        /// The root password; prompted for when omitted
        #[arg(long)]
        password: Option<String>,
    },
}

#[derive(Subcommand)]
enum InstanceAction {
    /// Register an installation directory
    Add {
        name: String,
        /// Installation root, the directory containing bin/mysqld.exe
        #[arg(long)]
        home: String,
        /// Option file, defaults to <home>/my.ini
        #[arg(long = "my-ini")]
        my_ini: Option<String>,
        /// Root password for this instance
        #[arg(long = "root-password")]
        root_password: Option<String>,
    },
    /// List registered instances
    List,
    /// Remove an instance from the configuration, without touching its files
    Remove { name: String },
}

#[derive(Subcommand)]
enum UserAction {
    Add {
        name: String,
        user: String,
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        password: String,
    },
    Remove {
        name: String,
        user: String,
        #[arg(long)]
        host: Option<String>,
        /// Skip the confirmation prompt
        #[arg(long)]
        yes: bool,
    },
    List {
        name: String,
    },
}

#[derive(Subcommand)]
enum DbAction {
    Add {
        name: String,
        database: String,
    },
    Remove {
        name: String,
        database: String,
        /// Skip the confirmation prompt
        #[arg(long)]
        yes: bool,
    },
    List {
        name: String,
    },
}

fn main() {
    let cli = Cli::parse();
    let runner = SystemRunner;
    match run(cli, &runner) {
        Ok(()) => {}
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(error.code());
        }
    }
}

fn run(cli: Cli, runner: &dyn Runner) -> Result<()> {
    let config = config::load()?;
    let mut ctx = commands::Ctx::new(config, runner);

    let Some(command) = cli.command else {
        return ui::run_menu(&mut ctx);
    };

    match command {
        Command::Instance { action } => match action {
            InstanceAction::Add {
                name,
                home,
                my_ini,
                root_password,
            } => println!(
                "{}",
                commands::add_instance(
                    &mut ctx,
                    &name,
                    &home,
                    my_ini.as_deref(),
                    root_password.as_deref()
                )?
            ),
            InstanceAction::List => {
                let rows = commands::list_instances(&ctx)?;
                if rows.is_empty() {
                    println!("No instances are registered yet.");
                    println!("Add one with: mm instance add <name> --home <path>");
                } else {
                    println!(
                        "{:<16} {:<10} {:<7} {:<13} DATADIR",
                        "NAME", "VERSION", "PORT", "STATUS"
                    );
                    for row in rows {
                        println!(
                            "{:<16} {:<10} {:<7} {:<13} {}",
                            row.name, row.version, row.port, row.status, row.datadir
                        );
                    }
                }
            }
            InstanceAction::Remove { name } => {
                println!("{}", commands::remove_instance(&mut ctx, &name)?)
            }
        },
        Command::Start { name } => println!("{}", commands::start_instance(&mut ctx, &name)?),
        Command::Stop { name } => println!("{}", commands::stop_instance(&mut ctx, &name)?),
        Command::Status { name } => match name {
            Some(name) => println!("{}", commands::instance_status(&ctx, &name)?),
            None => {
                let rows = commands::list_instances(&ctx)?;
                if rows.is_empty() {
                    println!("No instances are registered yet.");
                } else {
                    for row in rows {
                        println!("{}: {} (port {})", row.name, row.status, row.port);
                    }
                }
            }
        },
        Command::User { action } => match action {
            UserAction::Add {
                name,
                user,
                host,
                password,
            } => {
                let host = host.unwrap_or_else(|| commands::default_host().to_string());
                println!(
                    "{}",
                    commands::add_user(&mut ctx, &name, &user, &host, &password)?
                )
            }
            UserAction::Remove {
                name,
                user,
                host,
                yes,
            } => {
                let host = host.unwrap_or_else(|| commands::default_host().to_string());
                if !yes && !ui::ask_drop_confirmation("user", &format!("{user}@{host}"))? {
                    println!("Cancelled.");
                    return Ok(());
                }
                println!("{}", commands::remove_user(&mut ctx, &name, &user, &host)?)
            }
            UserAction::List { name } => {
                for account in commands::list_users(&ctx, &name)? {
                    println!("{account}");
                }
            }
        },
        Command::Db { action } => match action {
            DbAction::Add { name, database } => {
                println!("{}", commands::add_database(&mut ctx, &name, &database)?)
            }
            DbAction::Remove {
                name,
                database,
                yes,
            } => {
                if !yes && !ui::ask_drop_confirmation("database", &database)? {
                    println!("Cancelled.");
                    return Ok(());
                }
                println!("{}", commands::remove_database(&mut ctx, &name, &database)?)
            }
            DbAction::List { name } => {
                for schema in commands::list_databases(&ctx, &name)? {
                    println!("{schema}");
                }
            }
        },
        Command::Bind {
            name,
            database,
            user,
            host,
        } => {
            let host = host.unwrap_or_else(|| commands::default_host().to_string());
            println!(
                "{}",
                commands::bind(&mut ctx, &name, &database, &user, &host)?
            )
        }
        Command::Unbind { name, user, host } => {
            let host = host.unwrap_or_else(|| commands::default_host().to_string());
            println!("{}", commands::unbind(&mut ctx, &name, &user, &host)?)
        }
        Command::Workflow { name } => {
            let name = match name {
                Some(value) => value,
                None => ui::pick_instance_for_workflow(&ctx)?,
            };
            let outcome = commands::workflow(&mut ctx, &name)?;
            if outcome.started {
                println!("Started `{}` for this run.", outcome.instance);
            }
            println!("Test workflow completed successfully.");
            println!("  instance: {} (port {})", outcome.instance, outcome.port);
            println!("  database: {}", outcome.database);
            println!("  user:     {}@{}", outcome.username, outcome.host);
            println!("  password: {}", outcome.password);
            println!(
                "\nConnect with:\n  mysql -h 127.0.0.1 -P {} -u {} -p'{}' {}",
                outcome.port, outcome.username, outcome.password, outcome.database
            );
            println!("\nThese details were saved to the configuration file for later reference.");
        }
        Command::Login { name, password } => {
            let password = match password {
                Some(value) => value,
                None => ui::ask_password("Root password: ")?,
            };
            println!("{}", commands::login(&mut ctx, &name, &password)?)
        }
    }

    Ok(())
}
