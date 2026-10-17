use crate::commands::{self, Ctx};
use crate::error::{AppError, Result};
use std::io::{self, Write};

/// Reads a line from stdin, returning `None` at end of input.
fn read_line(prompt: &str) -> Result<Option<String>> {
    print!("{prompt}");
    io::stdout()
        .flush()
        .map_err(|error| AppError::general(format!("cannot write to the terminal: {error}")))?;
    let mut buffer = String::new();
    let read = io::stdin()
        .read_line(&mut buffer)
        .map_err(|error| AppError::general(format!("cannot read from the terminal: {error}")))?;
    if read == 0 {
        return Ok(None);
    }
    Ok(Some(buffer.trim_end_matches(['\r', '\n']).to_string()))
}

fn require_line(prompt: &str) -> Result<String> {
    read_line(prompt)?.ok_or_else(|| AppError::usage("input ended unexpectedly".to_string()))
}

/// Asks for a non-empty value, repeating until one is supplied.
fn require_non_empty(prompt: &str) -> Result<String> {
    loop {
        let value = require_line(prompt)?;
        if !value.trim().is_empty() {
            return Ok(value.trim().to_string());
        }
        println!("A value is required.");
    }
}

/// Presents a numbered list and returns the chosen index.
fn choose(prompt: &str, options: &[String]) -> Result<usize> {
    if options.is_empty() {
        return Err(AppError::usage("there is nothing to choose from".to_string()));
    }
    loop {
        println!();
        for (index, option) in options.iter().enumerate() {
            println!("  {}) {option}", index + 1);
        }
        let input = require_line(&format!("{prompt} [1-{}]: ", options.len()))?;
        match input.trim().parse::<usize>() {
            Ok(value) if value >= 1 && value <= options.len() => return Ok(value - 1),
            _ => println!("Enter a number between 1 and {}.", options.len()),
        }
    }
}

fn confirm(prompt: &str) -> Result<bool> {
    let input = require_line(&format!("{prompt} [y/N]: "))?;
    Ok(matches!(input.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}

/// Prompts for an instance, or returns the single registered one directly.
fn pick_instance(ctx: &Ctx<'_>) -> Result<String> {
    let names = commands::instance_names(&ctx.config);
    match names.len() {
        0 => Err(AppError::instance(
            "no instance is registered; run `mm instance add <name> --home <path>` first".to_string(),
        )),
        1 => Ok(names[0].clone()),
        _ => {
            let index = choose("Select an instance", &names)?;
            Ok(names[index].clone())
        }
    }
}

fn print_instances(ctx: &Ctx<'_>) -> Result<()> {
    let rows = commands::list_instances(ctx)?;
    if rows.is_empty() {
        println!("No instances are registered yet.");
        println!("Add one with: mm instance add <name> --home <path>");
        return Ok(());
    }
    println!();
    println!("{:<16} {:<10} {:<7} {:<13} DATADIR", "NAME", "VERSION", "PORT", "STATUS");
    for row in rows {
        println!(
            "{:<16} {:<10} {:<7} {:<13} {}",
            row.name, row.version, row.port, row.status, row.datadir
        );
    }
    Ok(())
}

pub fn run_menu(ctx: &mut Ctx<'_>) -> Result<()> {
    loop {
        println!();
        println!("MySQL Manager");
        println!("  1) List instances");
        println!("  2) Start an instance");
        println!("  3) Stop an instance");
        println!("  4) Create a database");
        println!("  5) Create a user");
        println!("  6) Bind a user to a database");
        println!("  7) One-shot test workflow");
        println!("  0) Exit");

        let input = match read_line("\nChoice: ")? {
            Some(value) => value,
            None => {
                println!();
                return Ok(());
            }
        };

        match input.trim() {
            "0" | "" => return Ok(()),
            "1" => {
                if let Err(error) = print_instances(ctx) {
                    eprintln!("error: {error}");
                }
            }
            "2" => match run_start(ctx) {
                Ok(message) => println!("{message}"),
                Err(error) => eprintln!("error: {error}"),
            },
            "3" => match run_stop(ctx) {
                Ok(message) => println!("{message}"),
                Err(error) => eprintln!("error: {error}"),
            },
            "4" => match run_create_database(ctx) {
                Ok(message) => println!("{message}"),
                Err(error) => eprintln!("error: {error}"),
            },
            "5" => match run_create_user(ctx) {
                Ok(message) => println!("{message}"),
                Err(error) => eprintln!("error: {error}"),
            },
            "6" => match run_bind(ctx) {
                Ok(message) => println!("{message}"),
                Err(error) => eprintln!("error: {error}"),
            },
            "7" => match run_workflow(ctx) {
                Ok(()) => {}
                Err(error) => eprintln!("error: {error}"),
            },
            other => println!("Unknown choice: {other}"),
        }
    }
}

fn run_start(ctx: &mut Ctx<'_>) -> Result<String> {
    let name = pick_instance(ctx)?;
    commands::start_instance(ctx, &name)
}

fn run_stop(ctx: &mut Ctx<'_>) -> Result<String> {
    let name = pick_instance(ctx)?;
    commands::stop_instance(ctx, &name)
}

fn run_create_database(ctx: &mut Ctx<'_>) -> Result<String> {
    let name = pick_instance(ctx)?;
    let database = require_non_empty("Database name: ")?;
    commands::add_database(ctx, &name, &database)
}

fn run_create_user(ctx: &mut Ctx<'_>) -> Result<String> {
    let name = pick_instance(ctx)?;
    let user = require_non_empty("User name: ")?;
    let host = {
        let value = require_line("Host [localhost]: ")?;
        if value.trim().is_empty() {
            commands::default_host().to_string()
        } else {
            value.trim().to_string()
        }
    };
    let password = require_non_empty("Password: ")?;
    commands::add_user(ctx, &name, &user, &host, &password)
}

fn run_bind(ctx: &mut Ctx<'_>) -> Result<String> {
    let name = pick_instance(ctx)?;
    let database = require_non_empty("Database to bind: ")?;
    let user = require_non_empty("User to bind: ")?;
    let host = {
        let value = require_line("Host [localhost]: ")?;
        if value.trim().is_empty() {
            commands::default_host().to_string()
        } else {
            value.trim().to_string()
        }
    };
    commands::bind(ctx, &name, &database, &user, &host)
}

fn run_workflow(ctx: &mut Ctx<'_>) -> Result<()> {
    let name = pick_instance(ctx)?;
    println!("\nPreparing a throwaway database and user on `{name}`...");
    let outcome = commands::workflow(ctx, &name)?;
    if outcome.started {
        println!("Started `{}` for this run.", outcome.instance);
    }
    println!();
    println!("Test workflow completed successfully.");
    println!("  instance: {} (port {})", outcome.instance, outcome.port);
    println!("  database: {}", outcome.database);
    println!("  user:     {}@{}", outcome.username, outcome.host);
    println!("  password: {}", outcome.password);
    println!();
    println!("Connect with:");
    println!(
        "  mysql -h 127.0.0.1 -P {} -u {} -p'{}' {}",
        outcome.port, outcome.username, outcome.password, outcome.database
    );
    println!();
    println!("These details were saved to the configuration file for later reference.");
    Ok(())
}

/// Reads a secret. Echo is not suppressed: doing so reliably on Windows needs
/// either an extra crate or raw console mode handling, and this tool targets
/// local development databases where the password is already stored in the
/// configuration file. The value is never logged.
pub fn ask_password(prompt: &str) -> Result<String> {
    require_non_empty(prompt)
}

/// Interactive variant of `mm workflow` used by the `workflow` subcommand when
/// the instance name is omitted.
pub fn pick_instance_for_workflow(ctx: &Ctx<'_>) -> Result<String> {
    pick_instance(ctx)
}

pub fn ask_drop_confirmation(kind: &str, name: &str) -> Result<bool> {
    confirm(&format!("Permanently drop the {kind} `{name}`?"))
}
