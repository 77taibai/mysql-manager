---
feature: mysql-manager
status: designed
updated: 2026-02-14
branch: main
commits:
---

# MySQL Manager (mm)

一个运行在 Windows 11 上的 Rust 命令行工具，用于管理本机多个 MySQL 实例的按需启停，以及库/用户的创建、删除与一对一权限绑定。

## Report

## [S1] Problem

本机需要同时保留多个 MySQL 版本用于开发测试，但存在三个现实问题：

1. 安装多个版本后，没有统一入口知道装了哪些、各自监听哪个端口、当前是否在跑。MySQL 官方安装包不提供这种跨版本视图。
2. 把 MySQL 注册成 Windows 服务会让所有版本常驻后台占用内存，而实际只在需要时用几分钟。需要按需启停，而不是开机自启。
3. 反复手工敲 `CREATE USER` / `CREATE DATABASE` / `GRANT` 容易出错，且极易因 `GRANT ALL ON *.*` 之类的宽授权让测试账号能碰到其它库。需要工具从权限层强制隔离。

工具要解决的正是这三点：一个可查询的实例清单、命令行的按需启停、以及带强制隔离的库/用户管理。

## [S2] Design

### [S2.1] 命令面

二进制名 `mm`。采用「CLI 子命令 + 无参数进入交互式菜单」的双入口形态。

```
mm                                     # 无参数：进入交互式菜单
mm instance add <name> --home <path>   # 手动登记一个 MySQL 安装
mm instance list                       # 列出已登记实例及其状态
mm instance remove <name>              # 从清单移除（不删文件）
mm start <name>                        # 后台启动
mm stop <name>                         # 停止
mm status [name]                       # 查看状态（缺省列出全部）
mm user add <name> <user>              # 创建用户
mm user remove <name> <user>
mm user list <name>
mm db add <name> <db>                  # 创建数据库
mm db remove <name> <db>
mm db list <name>
mm bind <name> <db> <user>             # 一对一独占绑定
mm unbind <name> <user>                # 解除绑定
mm workflow                            # 一键测试工作流
mm login <name>                        # 设置/更新 root 密码
```

`<name>` 指已登记的实例名，不是 MySQL 版本号。

### [S2.2] 实例登记（纯手动）

不做任何文件系统或注册表扫描。实例只来自 `mm instance add`。

登记时校验 `<home>/bin/mysqld.exe` 存在，否则报错并终止。登记内容：

- `name`：用户给定的唯一标识
- `home`：安装根目录（绝对路径）
- `my_ini`：可选，缺省为 `<home>/my.ini`
- `root_password`：该实例的 root 密码，明文存于配置文件

`datadir`、`port` 不在登记时保存，而是在每次使用时从 `my.ini` 现场解析。理由：用户明确要求数据目录以其手写配置为准，把值复制进工具配置会造成两处真相，用户在 `my.ini` 改端口后工具会读到陈旧值。

`my.ini` 解析只取 `[mysqld]` 段内的 `basedir`、`datadir`、`port` 三个键，容忍正反斜杠、行内注释、键值两侧空白。`datadir` 或 `port` 缺失时报错，指明缺失的键名与该 ini 路径。

### [S2.3] 配置文件

路径固定为**可执行文件同级目录**下的 `config.toml`，通过 `std::env::current_exe()` 的父目录推导，不使用当前工作目录。这样 `cargo run` 与直接运行 exe 行为一致（`cargo run` 时 exe 在 `target/debug/`，配置也落在那里）。

结构：

```toml
[[instances]]
name = "mysql56"
home = 'C:\Users\taiba\AppData\Local\Programs\MySQL\mysql-5.6.51-winx64'
my_ini = 'C:\...\my.ini'
root_password = "secret"
last_pid = 12345

[workflow]
last_run = '2026-02-14T10:00:00+08:00'
instance = "mysql56"
database = "db_a3f9c1"
username = "u_7b2e4d"
password = "Xk9#mQ2p"
```

配置文件缺失时视为空清单，首次 `instance add` 时创建。写入采用「先写临时文件再 rename」避免中途崩溃产生半截文件。

### [S2.4] 启停

**start**：读 `my.ini` 得 datadir 与 port → 若该实例已在运行则报错退出 → 以 `mysqld --defaults-file=<my_ini>` 启动，创建时不带控制台窗口、父进程退出后子进程继续存活 → 记录 PID 到配置 → 轮询 `mysqladmin ping` 直到就绪或超时（默认 30 秒）→ 超时则终止子进程并报错，附 datadir 下 `.err` 日志路径。

**stop**：优先 `mysqladmin -u root shutdown`；失败或超时则按记录的 PID 强制终止进程。成功后清除 `last_pid`。

**status**：`mysqladmin ping` 成功即 Running，否则 Stopped。同时校验 PID 存活，防止 PID 被系统复用造成误判。

不使用 Windows 服务，不写注册表，不设置开机自启。

### [S2.5] SQL 执行层

所有 SQL 通过实例自带的 `<home>/bin/mysql.exe` 执行，不引入网络驱动。

密码传递方式：生成临时 `--defaults-file` 文件（内容 `[client]\nuser=root\npassword=...\nport=...`），以该文件调用 mysql.exe，用完立即删除。避免密码出现在进程命令行被其它进程读取。

调用约定：`mysql.exe --defaults-file=<tmp> --batch --skip-column-names -e "<sql>"`，捕获 stdout/stderr 与退出码。非零退出码时把 stderr 原文带回给用户，不做改写，便于定位真实 SQL 错误。

**版本分支**：启动前用 `mysqld --version` 解析主版本号并缓存。

- `>= 8.0`：`CREATE USER IF NOT EXISTS '<u>'@'<h>' IDENTIFIED BY '<p>'`；授权后如需回收用 `REVOKE ALL PRIVILEGES, GRANT OPTION FROM`
- `< 8.0`（5.5/5.6/5.7）：`CREATE USER '<u>'@'<h>' IDENTIFIED BY '<p>'`（无 `IF NOT EXISTS`），建用户前先查 `mysql.user` 判断是否已存在
- `>= 5.7.6` 才支持 `ALTER USER ... IDENTIFIED BY`；5.6 改密走 `SET PASSWORD FOR`

所有标识符（库名、用户名）在拼接 SQL 前校验：仅允许 `[A-Za-z0-9_$]`，长度 1–64；密码用单引号包裹并转义内部 `'` 与 `\`。拒绝拼接未校验输入。

### [S2.6] 库与用户的增删

- `db add`：`CREATE DATABASE \`<db>\` CHARACTER SET utf8mb4`（5.5.3+ 支持 utf8mb4；更低版本回退 utf8）
- `db remove`：要求二次确认（交互式输入库名，或 `--yes`），执行 `DROP DATABASE`
- `user add`：按 [S2.5] 版本分支创建，host 默认 `localhost`，`--host` 可覆盖
- `user remove`：`DROP USER`；若该用户存在绑定，先提示将一并解除
- 两者默认都拒绝删除系统库（`mysql`、`information_schema`、`performance_schema`、`sys`）与 root 用户

### [S2.7] 一对一独占绑定

`mm bind <instance> <db> <user>` 的语义是**该用户只能访问该库**：

1. `GRANT ALL PRIVILEGES ON \`<db>\`.* TO '<user>'@'<host>'`
2. 回收该用户在所有其它库上的权限：遍历 `SHOW GRANTS FOR`，对每条形如 `GRANT ... ON \`<other>\`.* TO` 且 `<other> != <db>` 的授权执行对应 `REVOKE`；同时 `REVOKE ALL PRIVILEGES, GRANT OPTION FROM` 清掉全局级授权
3. 保留 `USAGE`（连接能力本身不算权限）
4. `FLUSH PRIVILEGES`

不授予 `GRANT OPTION`，不授予全局级权限。这样即使该账号密码泄露，也无法触碰其它库。

若该用户已被绑定到另一个库，`bind` 拒绝执行并提示先 `unbind`，避免静默改绑造成误解。

绑定关系记录在配置文件中（`[[bindings]]`，含 instance / database / user），仅用于展示与 `unbind` 定位，真实权限状态以 MySQL 的 `SHOW GRANTS` 为准。工具在 `bind`/`unbind` 后重新查询 `SHOW GRANTS` 并输出实际生效的权限，不假设操作一定成功。

### [S2.8] 交互式菜单与一键工作流

无参数运行 `mm` 进入菜单，选项覆盖：列出实例、启动、停止、建库、建用户、绑定、一键测试工作流、退出。菜单读取实例清单动态生成可选项，无实例时提示先 `instance add`。

`mm workflow` 一键测试工作流：

1. 列出已登记实例，交互式选择其一
2. 若未运行则启动，并等待就绪
3. 生成随机库名 `db_<6位小写字母数字>`、随机用户名 `u_<6位小写字母数字>`、随机密码（16 位，含大小写字母与数字，排除易混淆字符）
4. 建库 → 建用户（host=localhost）→ 一对一绑定
5. 用新账号实际连接一次（`mysql.exe -u <user> -p<pwd> -D <db> -e "SELECT 1"`）验证可用
6. 打印连接信息，并写入配置文件 `[workflow]` 段

随机名在写入前检查是否与现有库/用户重名，重名则重新生成。

### [S2.9] 错误处理与退出码

- `0` 成功；`1` 一般错误；`2` 参数/用法错误；`3` 实例未登记或路径无效；`4` MySQL 未就绪或连接失败
- 所有面向用户的错误写到 stderr，正常结果写 stdout
- 不在错误信息中回显密码；`--defaults-file` 临时文件即使出错也通过 RAII guard 删除

### [S2.10] 测试边界

- `my.ini` 解析、随机名生成、标识符校验、版本号解析、配置文件读写：纯函数，单元测试覆盖，不依赖真实 MySQL
- 进程启停与 SQL 执行：通过 trait 抽象成可替换的 executor，单元测试用 fake 实现验证命令构造与错误传播
- 真实 MySQL 5.6.51 的端到端验证作为人工验收步骤记录在报告里，不进自动化测试（避免测试依赖本机特定安装）

## [S3] Out of Scope

- 不做 MySQL 版本的自动下载、安装、升级
- 不做文件系统/注册表自动发现（用户明确选择纯手动登记）
- 不注册 Windows 服务，不设开机自启
- 不做图形界面
- 不做远程实例管理，仅本机
- 不做数据备份/恢复、慢查询分析、性能监控
- 不做 SQL 客户端交互式查询功能（只执行工具自身需要的管理语句）
- 不管理表结构
- 不做多实例并发编排（一次操作一个实例）

## Tasks

- [ ] T1: 建立 cargo 项目骨架，实现配置文件定位（`current_exe` 同级）与 `config.toml` 读写 — acceptance: `cargo build` 通过；单元测试验证配置往返读写与「文件不存在时返回空清单」 (covers: S2.3, S2.9)
- [ ] T2: 实现 `my.ini` 解析（`[mysqld]` 段的 basedir/datadir/port）与实例登记三命令 — acceptance: `mm instance add` 对有效目录成功、对缺少 `mysqld.exe` 的目录报错退出码 3；`mm instance list` 输出已登记项；单元测试覆盖 ini 解析的各种格式 (covers: S2.2)
- [ ] T3: 实现 start/stop/status，后台启动 mysqld、PID 记录、就绪轮询与强制终止 — acceptance: 对本机 5.6.51 执行 `mm start mysql56` 后 `mm status` 显示 Running，`mm stop` 后显示 Stopped；重复 start 报错 (covers: S2.4)
- [ ] T4: 实现 SQL 执行层，含版本分支、临时 defaults-file、标识符与密码转义 — acceptance: 单元测试验证 5.6 与 8.0 生成的 SQL 语句差异、非法标识符被拒、密码中的引号被正确转义 (covers: S2.5, S2.9)
- [ ] T5: 实现 user/db 增删与 `bind` 一对一独占绑定 — acceptance: 在本机 5.6.51 上建库建用户后绑定，`SHOW GRANTS` 仅含该库权限；`bind` 到第二个库被拒绝；系统库与 root 的删除被拒 (covers: S2.6, S2.7)
- [ ] T6: 实现无参数交互式菜单与 `mm workflow` 一键工作流 — acceptance: `mm workflow` 完成选实例→启动→随机建库建用户→绑定→新账号连通验证→打印并落盘；配置文件中出现 `[workflow]` 段 (covers: S2.8)
- [ ] T7: 补齐错误码一致性、密码不回显、临时文件清理，并跑通 `cargo test` / `cargo clippy` / `cargo build --release` — acceptance: 三条命令均无 error；clippy 无 warning (covers: S2.9, S2.10)
