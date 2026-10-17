# mm — Windows MySQL Instance Manager

A command-line tool for Windows 11 that manages multiple local MySQL installations: start and stop them on demand, and create databases and users with enforced one-to-one isolation.

## Why

- **No service, no background cost.** MySQL is started only when you ask for it and stopped when you are done, so idle versions do not sit in memory.
- **One view across versions.** A single list shows every registered installation, its version, port, datadir, and whether it is running.
- **Isolation by privilege, not by convention.** Binding a user to a database grants that schema and revokes everything else, so a test account cannot touch your other databases.

## Build

```powershell
cargo build --release
```

The binary is `target\release\mm.exe`. Configuration is stored in `config.toml` **next to the executable**, so copy both if you move the tool.

## Usage

Register an installation (the directory containing `bin\mysqld.exe`):

```powershell
mm instance add mysql56 --home "C:\Users\me\AppData\Local\Programs\MySQL\mysql-5.6.51-winx64"
mm instance add mysql56 --home "C:\mysql\5.6" --my-ini "D:\configs\my56.ini" --root-password secret
```

Record the root password so other commands can connect:

```powershell
mm login mysql56 --password 123456
```

Day to day:

```powershell
mm instance list          # name, version, port, status, datadir
mm start mysql56          # background start, returns once the server answers
mm status                 # Running / Stopped for every instance
mm stop mysql56

mm db add mysql56 appdb
mm user add mysql56 appuser --password secret
mm bind mysql56 appdb appuser    # appuser can now reach only appdb
mm unbind mysql56 appuser

mm db remove mysql56 appdb --yes
mm user remove mysql56 appuser --yes

mm workflow               # interactive end-to-end smoke test
mm                        # interactive menu
```

`mm workflow` picks an instance, starts it if needed, generates a random database, user, and password, binds them, verifies the new account can actually connect, prints the connection details, and saves them to `config.toml`.

## Design notes

- **Datadir comes from your `my.ini`.** `datadir`, `port`, and `basedir` are read from the `[mysqld]` section on every run, so editing the option file is always authoritative. The tool never keeps a second copy that could drift.
- **`bind` is exclusive.** It grants `ALL PRIVILEGES` on the target schema, revokes global privileges, and then re-reads `SHOW GRANTS` to prove no privilege remains elsewhere. Binding a user who is already bound to another database is refused; run `mm unbind` first.
- **Passwords never touch a command line.** SQL runs through the instance's own `mysql.exe` with a temporary `--defaults-file` that is deleted immediately after use.
- **Version-aware SQL.** `CREATE USER IF NOT EXISTS` and `utf8mb4` are used only where the server supports them (5.7.6+ / 5.5.3+); MySQL 8 accounts get `mysql_native_password` so older clients can connect.
- **Protected objects.** The `mysql`, `information_schema`, `performance_schema`, and `sys` schemas and the `root` account cannot be dropped or bound.

## A note on PowerShell

`mm start` launches `mysqld` as a detached process. When a PowerShell pipeline is attached to the command (for example `mm start mysql56 2>&1 | Out-String`), PowerShell may keep waiting on the pipeline even though `mm.exe` has already exited and the server is running. This is a PowerShell behaviour, not a hang in `mm`. Run `start` directly, or redirect through `cmd`:

```powershell
cmd /c "mm.exe start mysql56"
```

## Exit codes

| Code | Meaning |
| ---- | ------- |
| 0 | Success |
| 1 | General error |
| 2 | Usage error, or a refused destructive operation |
| 3 | Instance not registered, or invalid installation path |
| 4 | MySQL unreachable, or the server rejected a command |

## Requirements

- Windows 10/11, x64
- One or more existing MySQL installations (this tool does not download or install MySQL)
- Rust toolchain only for building
