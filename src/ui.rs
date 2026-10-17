use crate::commands::{self, Ctx};
use crate::error::{AppError, Result};
use std::io::{self, Write};

/// Reads a line from stdin, returning `None` at end of input.
fn read_line(prompt: &str) -> Result<Option<String>> {
    print!("{prompt}");
    io::stdout()
        .flush()
        .map_err(|error| AppError::general(format!("无法写入终端：{error}")))?;
    let mut buffer = String::new();
    let read = io::stdin()
        .read_line(&mut buffer)
        .map_err(|error| AppError::general(format!("无法读取终端输入：{error}")))?;
    if read == 0 {
        return Ok(None);
    }
    Ok(Some(buffer.trim_end_matches(['\r', '\n']).to_string()))
}

fn require_line(prompt: &str) -> Result<String> {
    read_line(prompt)?.ok_or_else(|| AppError::usage("输入意外结束".to_string()))
}

/// Asks for a non-empty value, repeating until one is supplied.
fn require_non_empty(prompt: &str) -> Result<String> {
    loop {
        let value = require_line(prompt)?;
        if !value.trim().is_empty() {
            return Ok(value.trim().to_string());
        }
        println!("该项不能为空，请重新输入。");
    }
}

/// Presents a numbered list and returns the chosen index.
fn choose(prompt: &str, options: &[String]) -> Result<usize> {
    if options.is_empty() {
        return Err(AppError::usage("没有可选项".to_string()));
    }
    loop {
        println!();
        for (index, option) in options.iter().enumerate() {
            println!("  {}) {option}", index + 1);
        }
        let input = require_line(&format!("{prompt} [1-{}]: ", options.len()))?;
        match input.trim().parse::<usize>() {
            Ok(value) if value >= 1 && value <= options.len() => return Ok(value - 1),
            _ => println!("请输入 1 到 {} 之间的数字。", options.len()),
        }
    }
}

fn confirm(prompt: &str) -> Result<bool> {
    let input = require_line(&format!("{prompt} [y/N]: "))?;
    Ok(matches!(
        input.trim().to_ascii_lowercase().as_str(),
        "y" | "yes" | "是"
    ))
}

/// Prompts for an instance. The menu is always shown, even when only one
/// instance is registered, so the target of an action is always explicit.
fn pick_instance(ctx: &Ctx<'_>) -> Result<String> {
    let names = commands::instance_names(&ctx.config);
    if names.is_empty() {
        return Err(AppError::instance(
            "尚未登记任何实例，请先运行 `mm instance add <名称> --home <路径>`".to_string(),
        ));
    }
    let index = choose("请选择实例", &names)?;
    Ok(names[index].clone())
}

fn print_instances(ctx: &Ctx<'_>) -> Result<()> {
    let rows = commands::list_instances(ctx)?;
    if rows.is_empty() {
        println!("尚未登记任何实例。");
        println!("可用以下命令添加：mm instance add <名称> --home <路径>");
        return Ok(());
    }
    println!();
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
    Ok(())
}

pub fn run_menu(ctx: &mut Ctx<'_>) -> Result<()> {
    loop {
        println!();
        println!("MySQL 管理器");
        println!("  1) 查看实例列表");
        println!("  2) 启动实例");
        println!("  3) 停止实例");
        println!("  4) 创建数据库");
        println!("  5) 创建用户");
        println!("  6) 绑定用户到数据库");
        println!("  7) 一键测试工作流");
        println!("  8) 查看 workflow 记录");
        println!("  9) 删除 workflow 记录");
        println!("  0) 退出");

        let input = match read_line("\n请选择：")? {
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
                    eprintln!("错误：{error}");
                }
            }
            "2" => match run_start(ctx) {
                Ok(message) => println!("{message}"),
                Err(error) => eprintln!("错误：{error}"),
            },
            "3" => match run_stop(ctx) {
                Ok(message) => println!("{message}"),
                Err(error) => eprintln!("错误：{error}"),
            },
            "4" => match run_create_database(ctx) {
                Ok(message) => println!("{message}"),
                Err(error) => eprintln!("错误：{error}"),
            },
            "5" => match run_create_user(ctx) {
                Ok(message) => println!("{message}"),
                Err(error) => eprintln!("错误：{error}"),
            },
            "6" => match run_bind(ctx) {
                Ok(message) => println!("{message}"),
                Err(error) => eprintln!("错误：{error}"),
            },
            "7" => match run_workflow(ctx) {
                Ok(()) => {}
                Err(error) => eprintln!("错误：{error}"),
            },
            "8" => {
                if let Err(error) = print_workflows(ctx) {
                    eprintln!("错误：{error}");
                }
            }
            "9" => match run_remove_workflow(ctx) {
                Ok(message) => println!("{message}"),
                Err(error) => eprintln!("错误：{error}"),
            },
            other => println!("无效的选项：{other}"),
        }
    }
}

fn run_start(ctx: &mut Ctx<'_>) -> Result<String> {
    let name = pick_instance(ctx)?;
    commands::start_instance(ctx, &name)
}

/// Prints the workflow history, including the generated passwords so an
/// earlier run's credentials stay recoverable.
fn print_workflows(ctx: &Ctx<'_>) -> Result<()> {
    let rows = commands::list_workflows(ctx);
    if rows.is_empty() {
        println!("暂无 workflow 记录。");
        println!("运行「一键测试工作流」可创建一条。");
        return Ok(());
    }
    println!();
    println!(
        "{:<5} {:<14} {:<12} {:<16} {:<22} 密码",
        "序号", "时间", "实例", "数据库", "用户"
    );
    for row in &rows {
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
    println!();
    println!("查看单条完整信息请用：mm workflows show <序号>");
    Ok(())
}

fn run_remove_workflow(ctx: &mut Ctx<'_>) -> Result<String> {
    let rows = commands::list_workflows(ctx);
    if rows.is_empty() {
        return Err(AppError::usage("暂无 workflow 记录可删除".to_string()));
    }
    let labels: Vec<String> = rows
        .iter()
        .map(|row| {
            format!(
                "{}) {} · {} · {}@{}",
                row.index, row.instance, row.database, row.username, row.host
            )
        })
        .collect();
    let choice = choose("请选择要删除的记录", &labels)?;
    let index = rows[choice].index;
    if !confirm(&format!(
        "将删除数据库 `{}` 与用户 `{}`@`{}`，确定吗？",
        rows[choice].database, rows[choice].username, rows[choice].host
    ))? {
        return Ok("已取消。".to_string());
    }
    commands::remove_workflow(ctx, index)
}

fn run_stop(ctx: &mut Ctx<'_>) -> Result<String> {
    let name = pick_instance(ctx)?;
    commands::stop_instance(ctx, &name)
}

fn run_create_database(ctx: &mut Ctx<'_>) -> Result<String> {
    let name = pick_instance(ctx)?;
    let database = require_non_empty("数据库名称：")?;
    commands::add_database(ctx, &name, &database)
}

fn run_create_user(ctx: &mut Ctx<'_>) -> Result<String> {
    let name = pick_instance(ctx)?;
    let user = require_non_empty("用户名：")?;
    let host = {
        let value = require_line("主机 [localhost]：")?;
        if value.trim().is_empty() {
            commands::default_host().to_string()
        } else {
            value.trim().to_string()
        }
    };
    let password = require_non_empty("密码：")?;
    commands::add_user(ctx, &name, &user, &host, &password)
}

fn run_bind(ctx: &mut Ctx<'_>) -> Result<String> {
    let name = pick_instance(ctx)?;
    let database = require_non_empty("要绑定的数据库：")?;
    let user = require_non_empty("要绑定的用户：")?;
    let host = {
        let value = require_line("主机 [localhost]：")?;
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
    println!("\n正在为 `{name}` 准备临时数据库和用户……");
    let outcome = commands::workflow(ctx, &name)?;
    if outcome.started {
        println!("本次运行已启动 `{}`。", outcome.instance);
    }
    print_workflow_result(&outcome);
    Ok(())
}

/// Prints the credentials produced by a workflow run.
pub fn print_workflow_result(outcome: &commands::WorkflowOutcome) {
    println!();
    println!("测试工作流执行成功。");
    println!("  实例：  {}（端口 {}）", outcome.instance, outcome.port);
    println!("  数据库：{}", outcome.database);
    println!("  用户：  {}@{}", outcome.username, outcome.host);
    println!("  密码：  {}", outcome.password);
    println!();
    println!("可用以下命令连接：");
    println!(
        "  mysql -h 127.0.0.1 -P {} -u {} -p'{}' {}",
        outcome.port, outcome.username, outcome.password, outcome.database
    );
    println!();
    println!("以上信息已保存到配置文件，便于后续查看。");
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
    confirm(&format!("确定要永久删除{kind} `{name}` 吗？"))
}
