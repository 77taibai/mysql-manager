---
name: mysql-manager
description: Use when the user wants to manage local MySQL instances on Windows with the mm command — starting/stopping an instance, listing or registering installs, creating/removing databases and users, binding a user exclusively to one database, running the one-click workflow, or recalling generated credentials. Triggers include "mm start", "mm instance", "mm workflow", "mysql-manager", "start MySQL instance", "on-demand MySQL".
---

# mysql-manager (mm)

`mm` is a Windows command-line manager for local MySQL instances. It starts and
stops multiple registered MySQL installs on demand and forces one-to-one
isolation of a user to a single database. Invoke `mm` directly from PowerShell.

## Prerequisites

- Platform: Windows. Use **PowerShell** syntax for all commands.
- Instances must be **registered manually** (`mm instance add`); the tool does not
  scan the filesystem/registry and does not download or install MySQL.
- Run `mm login` first (or pass `--root-password` to `add`) to record an
  instance's root password; connection-based commands need it.
- Config lives in `config.toml` **next to `mm.exe`** (not the current working
  directory); copy that file along when moving the tool.
- `datadir`/`port`/`basedir` are read live from the instance's `my.ini`
  (`[mysqld]` section) on every use, so editing `my.ini` takes effect immediately.

## Command surface

Running `mm` with no arguments opens an interactive menu; with a subcommand it is
scriptable. `<name>` is a registered instance name, not a MySQL version.

```powershell
# Register and inspect instances
mm instance add <name> --home <install-root> [--my-ini <path>] [--root-password <pwd>]
mm instance list
mm instance remove <name>          # remove from the list only, does not delete files

# Record/update the root password (needed by later commands)
mm login <name> --password <pwd>   # omit --password for interactive input

# Start/stop and status
mm start <name>                    # start in background, returns once ready
mm stop <name>
mm status [name]                   # lists all when name is omitted

# Databases
mm db add <name> <db>
mm db remove <name> <db> [--yes]
mm db clear <name> <db> [--yes]     # empty the schema, keep its name and credentials
mm db list <name>

# Users (host defaults to localhost)
mm user add <name> <user> --password <pwd> [--host <host>]
mm user remove <name> <user> [--host <host>] [--yes]
mm user list <name>

# Exclusive one-to-one binding
mm bind <name> <db> <user> [--host <host>]
mm unbind <name> <user> [--host <host>]

# Wipe the instance back to a fresh state
mm reset <name> [--yes]            # drops every user database and account

# One-click test workflow
mm workflow [name]                 # interactively picks an instance when name is omitted
mm workflows list                  # all records (including passwords)
mm workflows show <index>          # full details of one record
mm workflows remove <index>        # delete the record and its database and user

mm                                 # interactive menu
```

`mm instance add --home` must contain `bin\mysqld.exe`, otherwise it errors (exit
code 3).

## Typical flows

First-time setup:

```powershell
mm instance add mysql56 --home "C:\Users\me\AppData\Local\Programs\MySQL\mysql-5.6.51-winx64"
mm login mysql56 --password 123456
mm instance list
mm start mysql56
mm status
```

Create a database and user, then isolate them:

```powershell
mm db add mysql56 appdb
mm user add mysql56 appuser --password secret
mm bind mysql56 appdb appuser     # appuser can now access only appdb
```

Generate and verify a random credential set (records are appended, never
overwritten):

```powershell
mm workflow mysql56
mm workflows list                 # recover any password generated earlier
mm workflows remove 2             # also drops the real database and user
```

## Important constraints and gotchas

- **Only one instance runs at a time.** `start` checks whether any other
  registered instance is running (by connectivity, not by comparing ports) and
  refuses, telling you which one to stop first. `workflow` uses the same check.
- **`bind` is exclusive and revokes global privileges.** If the target account
  already holds any global privilege (`ON *.*`, including management accounts
  such as root), it is refused; users already bound to another database are also
  refused and must `mm unbind` first. After binding, only `USAGE` plus
  `ALL PRIVILEGES` on the target database remain.
- **Protected objects**: the system databases `mysql`, `information_schema`,
  `performance_schema`, `sys` and the accounts `root`, `mysql.sys`,
  `mysql.session` cannot be dropped or bound (exit code 2).
- **`mm db clear` empties a schema without dropping it.** Tables, views,
  procedures/functions and events are removed; the database itself, its charset,
  the accounts and their grants all stay, so credentials a `workflow` run printed
  keep working. Use it to reuse one test database repeatedly. Foreign keys do not
  block it (`FOREIGN_KEY_CHECKS` is toggled off, then restored). It re-reads the
  schema afterwards and fails rather than claiming success if anything survived.
  System schemas are refused; a missing schema is rejected before any DDL runs.
- **`mm reset` is a one-way wipe.** It drops every non-system database and every
  non-system account (anything not `root`/`mysql.*`), then clears that instance's
  bindings and workflow records from `config.toml`. The four system schemas and
  the shipped accounts are kept — removing them would break the instance rather
  than reset it. The server must be running. Confirmation requires retyping the
  instance name; pass `--yes` in scripts. The output lists exactly what was
  dropped and what was kept.
- **`db remove` / `user remove` ask for confirmation by default** (type the name);
  add `--yes` in scripts to skip.
- **`workflows remove` drops the user before the database**; the order must not
  be reversed.
- **PowerShell pipe gotcha**: `mm start` launches `mysqld` as a detached process,
  so piping its output (e.g. `mm start mysql56 2>&1 | Out-String`) can make
  PowerShell wait forever. Run it directly, or use:
  ```powershell
  cmd /c "mm start mysql56"
  ```
- **Passwords never hit the command line**: SQL runs through the instance's own
  `mysql.exe`, the root password goes via a temporary `--defaults-file` (deleted
  right after use), and DDL is fed through stdin.
- **Identifier validation**: database/user names allow only `[A-Za-z0-9_$]`,
  length 1–64.
- Console encoding is switched to UTF-8 (65001) at startup.

## Exit codes

| Code | Meaning |
| ---- | ------- |
| 0 | Success |
| 1 | General error |
| 2 | Usage error, or a destructive operation was refused |
| 3 | Instance not registered, or install path invalid |
| 4 | MySQL unreachable, or the server rejected the command |
