# mm — Windows MySQL 实例管理器

一个运行在 Windows 11 上的命令行工具，用于管理本机多个 MySQL 安装：按需启停，以及创建数据库和用户并强制一对一隔离。

## 为什么需要它

- **不注册服务，不长期占内存。** MySQL 只在你需要时启动、用完即停，闲置版本不会常驻后台。
- **跨版本统一视图。** 一个列表就能看到所有已登记安装的版本、端口、数据目录以及是否在运行。
- **靠权限隔离，不靠约定。** 把用户绑定到数据库时会只授予该库、回收其它所有权限，测试账号碰不到你别的数据库。

## 构建

```powershell
cargo build --release
```

产物是 `target\release\mm.exe`。配置文件 `config.toml` 存放在**可执行文件同级目录**，迁移时请一并拷贝。

## 用法

登记一个安装（即包含 `bin\mysqld.exe` 的目录）：

```powershell
mm instance add mysql56 --home "C:\Users\me\AppData\Local\Programs\MySQL\mysql-5.6.51-winx64"
mm instance add mysql56 --home "C:\mysql\5.6" --my-ini "D:\configs\my56.ini" --root-password secret
```

记录 root 密码，后续命令才能连接：

```powershell
mm login mysql56 --password 123456
```

日常操作：

```powershell
mm instance list          # 名称、版本、端口、状态、数据目录
mm start mysql56          # 后台启动，服务就绪后返回
mm status                 # 查看每个实例是 运行中 / 已停止
mm stop mysql56

mm db add mysql56 appdb
mm user add mysql56 appuser --password secret
mm bind mysql56 appdb appuser    # appuser 从此只能访问 appdb
mm unbind mysql56 appuser

mm db remove mysql56 appdb --yes
mm user remove mysql56 appuser --yes

mm workflow               # 交互式一键测试
mm                        # 交互式菜单
```

`mm workflow` 会选一个实例、必要时启动它、随机生成数据库/用户/密码、完成绑定、验证新账号确实能连上，然后打印连接信息并写入 `config.toml`。

## 设计说明

- **数据目录以你的 `my.ini` 为准。** `datadir`、`port`、`basedir` 每次都从 `[mysqld]` 段现读，改配置文件立即生效。工具不另存一份副本，避免两处不一致。
- **`bind` 是独占的。** 它先确认目标账号没有全局权限，再授予目标库 `ALL PRIVILEGES`、回收全局权限，最后回读 `SHOW GRANTS` 证明别处没有残留权限。已绑定到其它库的用户会被拒绝，需先 `mm unbind`。
- **密码不进命令行。** SQL 通过实例自带的 `mysql.exe` 执行，root 密码走临时 `--defaults-file`（用完即删），DDL 脚本经标准输入传入。
- **按版本生成 SQL。** `CREATE USER IF NOT EXISTS` 与 `utf8mb4` 只在服务端支持时使用（5.7.6+ / 5.5.3+）；MySQL 8 的账号使用 `mysql_native_password` 以兼容旧客户端。
- **受保护对象。** `mysql`、`information_schema`、`performance_schema`、`sys` 这些系统库以及 `root` 账号，都不能被删除或绑定。

## 关于 PowerShell 的提示

`mm start` 以脱离进程方式启动 `mysqld`。如果给命令接了 PowerShell 管道（例如 `mm start mysql56 2>&1 | Out-String`），即使 `mm.exe` 已经退出、服务已在运行，PowerShell 仍可能一直等待该管道。这是 PowerShell 自身的行为，不是 `mm` 卡住。请直接运行 `start`，或改用 `cmd` 重定向：

```powershell
cmd /c "mm.exe start mysql56"
```

## 中文显示

程序启动时会把控制台代码页切换为 UTF-8（65001），因此中文能正常显示。若你手动改过代码页，重新运行程序即可恢复。

## 退出码

| 退出码 | 含义 |
| ------ | ---- |
| 0 | 成功 |
| 1 | 一般错误 |
| 2 | 用法错误，或拒绝了破坏性操作 |
| 3 | 实例未登记，或安装路径无效 |
| 4 | MySQL 无法连接，或服务拒绝了命令 |

## 环境要求

- Windows 10/11 x64
- 一个或多个已安装的 MySQL（本工具不负责下载或安装 MySQL）
- 仅构建时需要 Rust 工具链
