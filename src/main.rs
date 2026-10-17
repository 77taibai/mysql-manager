mod client;
mod commands;
mod config;
mod console;
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
    about = "管理本机 MySQL 实例：按需启停，管理数据库与用户",
    long_about = None,
    help_template = "{about-with-newline}\n用法：{usage}\n\n{all-args}{after-help}",
    subcommand_help_heading = "命令",
    subcommand_value_name = "命令",
    disable_help_subcommand = true,
    disable_help_flag = true,
    disable_version_flag = true
)]
struct Cli {
    /// 显示帮助信息
    #[arg(short = 'h', long = "help", action = clap::ArgAction::Help, help_heading = "选项")]
    help: Option<bool>,

    /// 显示版本信息
    #[arg(
        short = 'V',
        long = "version",
        action = clap::ArgAction::Version,
        help_heading = "选项"
    )]
    version: Option<bool>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// 登记、查看或移除 MySQL 安装
    Instance {
        #[command(subcommand)]
        action: InstanceAction,
    },
    /// 在后台启动一个实例
    Start { name: String },
    /// 停止正在运行的实例
    Stop { name: String },
    /// 查看一个或全部实例的状态
    Status { name: Option<String> },
    /// 管理数据库账号
    User {
        #[command(subcommand)]
        action: UserAction,
    },
    /// 管理数据库
    Db {
        #[command(subcommand)]
        action: DbAction,
    },
    /// 将用户绑定到唯一一个数据库，并回收其它所有权限
    Bind {
        name: String,
        database: String,
        user: String,
        /// 账号主机，默认为 localhost
        #[arg(long)]
        host: Option<String>,
    },
    /// 解除绑定
    Unbind {
        name: String,
        user: String,
        #[arg(long)]
        host: Option<String>,
    },
    /// 执行一键测试工作流
    Workflow { name: Option<String> },
    /// 查看或删除历史 workflow 记录
    Workflows {
        #[command(subcommand)]
        action: WorkflowAction,
    },
    /// 设置或更新某个实例的 root 密码
    Login {
        name: String,
        /// root 密码；省略时交互输入
        #[arg(long)]
        password: Option<String>,
    },
}

#[derive(Subcommand)]
enum InstanceAction {
    /// 登记一个安装目录
    Add {
        name: String,
        /// 安装根目录，即包含 bin/mysqld.exe 的目录
        #[arg(long)]
        home: String,
        /// 配置文件，默认为 <home>/my.ini
        #[arg(long = "my-ini")]
        my_ini: Option<String>,
        /// 该实例的 root 密码
        #[arg(long = "root-password")]
        root_password: Option<String>,
    },
    /// 列出已登记的实例
    List,
    /// 从配置中移除实例，不改动其文件
    Remove { name: String },
}

#[derive(Subcommand)]
enum WorkflowAction {
    /// 列出全部 workflow 记录
    List,
    /// 查看单条记录的完整信息（含密码）
    Show { index: usize },
    /// 删除记录，同时删除其数据库与用户
    Remove { index: usize },
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
        /// 跳过确认提示
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
        /// 跳过确认提示
        #[arg(long)]
        yes: bool,
    },
    List {
        name: String,
    },
}

fn main() {
    console::use_utf8();
    let cli = Cli::parse();
    let runner = SystemRunner;
    match run(cli, &runner) {
        Ok(()) => {}
        Err(error) => {
            eprintln!("错误：{error}");
            std::process::exit(error.code());
        }
    }
}

fn print_instance_table(rows: &[commands::InstanceRow]) {
    if rows.is_empty() {
        println!("尚未登记任何实例。");
        println!("可用以下命令添加：mm instance add <名称> --home <路径>");
        return;
    }
    println!(
        "{:<16} {:<10} {:<7} {:<13} 数据目录",
        "名称", "版本", "端口", "状态"
    );
    for row in rows {
        println!(
            "{:<16} {:<10} {:<7} {:<13} {}",
            row.name, row.version, row.port, row.status, row.datadir
        );
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
                print_instance_table(&rows);
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
                    println!("尚未登记任何实例。");
                } else {
                    for row in rows {
                        println!("{}：{}（端口 {}）", row.name, row.status, row.port);
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
                if !yes && !ui::ask_drop_confirmation("用户", &format!("{user}@{host}"))? {
                    println!("已取消。");
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
                if !yes && !ui::ask_drop_confirmation("数据库", &database)? {
                    println!("已取消。");
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
                println!("本次运行已启动 `{}`。", outcome.instance);
            }
            ui::print_workflow_result(&outcome);
        }
        Command::Workflows { action } => match action {
            WorkflowAction::List => {
                let rows = commands::list_workflows(&ctx);
                if rows.is_empty() {
                    println!("暂无 workflow 记录。");
                    println!("运行 `mm workflow` 可创建一条。");
                } else {
                    println!(
                        "{:<5} {:<14} {:<12} {:<16} {:<22} 密码",
                        "序号", "时间", "实例", "数据库", "用户"
                    );
                    for row in rows {
                        println!(
                            "{:<5} {:<14} {:<12} {:<16} {:<22} {}",
                            row.index,
                            row.last_run,
                            row.instance,
                            row.database,
                            format!("{}@{}", row.username, row.host),
                            row.password
                        );
                    }
                }
            }
            WorkflowAction::Show { index } => {
                let row = commands::show_workflow(&ctx, index)?;
                println!("workflow 记录 {index}");
                println!("  时间：  {}", row.last_run);
                println!("  实例：  {}", row.instance);
                println!("  数据库：{}", row.database);
                println!("  用户：  {}@{}", row.username, row.host);
                println!("  密码：  {}", row.password);
            }
            WorkflowAction::Remove { index } => {
                println!("{}", commands::remove_workflow(&mut ctx, index)?)
            }
        },
        Command::Login { name, password } => {
            let password = match password {
                Some(value) => value,
                None => ui::ask_password("root 密码：")?,
            };
            println!("{}", commands::login(&mut ctx, &name, &password)?)
        }
    }

    Ok(())
}
