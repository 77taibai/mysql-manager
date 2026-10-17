use crate::error::{AppError, Result};
use rand::Rng;

/// System schemas that the tool refuses to drop.
pub const PROTECTED_SCHEMAS: [&str; 4] =
    ["mysql", "information_schema", "performance_schema", "sys"];

/// Accounts that the tool refuses to drop.
pub const PROTECTED_USERS: [&str; 3] = ["root", "mysql.sys", "mysql.session"];

pub const MAX_IDENTIFIER_LEN: usize = 64;

/// Accepts only `[A-Za-z0-9_$]`, 1..=64 characters. Anything else is rejected
/// before it can reach a SQL string, so no user input is ever interpolated
/// unvalidated.
pub fn validate_identifier(kind: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(AppError::usage(format!("{kind}名称不能为空")));
    }
    if value.len() > MAX_IDENTIFIER_LEN {
        return Err(AppError::usage(format!(
            "{kind}名称 `{value}` 超过 {MAX_IDENTIFIER_LEN} 个字符"
        )));
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '$')
    {
        return Err(AppError::usage(format!(
            "{kind}名称 `{value}` 含不支持的字符；只能使用字母、数字、`_` 或 `$`"
        )));
    }
    Ok(())
}

pub fn validate_schema_name(value: &str) -> Result<()> {
    validate_identifier("database", value)
}

pub fn validate_user_name(value: &str) -> Result<()> {
    validate_identifier("user", value)
}

/// Host values reach SQL inside quotes, so only allow the characters that a
/// legitimate host or `%` wildcard needs.
pub fn validate_host(value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(AppError::usage("主机不能为空".to_string()));
    }
    if value.len() > 255 {
        return Err(AppError::usage("主机过长".to_string()));
    }
    if !value.chars().all(|ch| {
        ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | '%' | ':' | '[' | ']')
    }) {
        return Err(AppError::usage(format!("主机 `{value}` 含不支持的字符")));
    }
    Ok(())
}

pub fn is_protected_schema(value: &str) -> bool {
    PROTECTED_SCHEMAS
        .iter()
        .any(|item| item.eq_ignore_ascii_case(value))
}

pub fn is_protected_user(value: &str) -> bool {
    PROTECTED_USERS
        .iter()
        .any(|item| item.eq_ignore_ascii_case(value))
}

/// Quotes a string literal for MySQL, escaping backslashes and single quotes.
pub fn quote_literal(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('\'');
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '\'' => escaped.push_str("\\'"),
            '\0' => escaped.push_str("\\0"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\x1a' => escaped.push_str("\\Z"),
            other => escaped.push(other),
        }
    }
    escaped.push('\'');
    escaped
}

/// Wraps an already validated identifier in backticks.
pub fn quote_ident(value: &str) -> String {
    format!("`{value}`")
}

/// Wraps a validated user/host pair.
pub fn account(user: &str, host: &str) -> String {
    format!("{}@{}", quote_literal(user), quote_literal(host))
}

/// Server capability derived from `mysqld --version`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ServerVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl ServerVersion {
    pub fn parse(text: &str) -> Option<Self> {
        // `mysqld --version` prefixes argv[0], so a path like `D:\mysql8\bin`
        // would otherwise be mistaken for the version. Anchor on the banner
        // token when present and only fall back to a bare number otherwise.
        let candidate = match text.rfind("Ver ") {
            Some(index) => &text[index + 4..],
            None => text,
        };
        let start = candidate.find(|ch: char| ch.is_ascii_digit())?;
        let rest = &candidate[start..];
        let token: String = rest
            .chars()
            .take_while(|ch| ch.is_ascii_digit() || *ch == '.')
            .collect();
        let mut parts = token.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next().unwrap_or("0").parse().unwrap_or(0);
        let patch = parts
            .next()
            .map(|value| {
                let digits: String = value.chars().take_while(|ch| ch.is_ascii_digit()).collect();
                digits.parse().unwrap_or(0)
            })
            .unwrap_or(0);
        Some(Self {
            major,
            minor,
            patch,
        })
    }

    /// `CREATE USER IF NOT EXISTS` exists from 5.7.6 onwards.
    pub fn supports_create_user_if_not_exists(&self) -> bool {
        *self
            >= Self {
                major: 5,
                minor: 7,
                patch: 6,
            }
    }

    /// `DROP USER IF EXISTS` exists from 5.7.8 onwards.
    pub fn supports_drop_user_if_exists(&self) -> bool {
        *self
            >= Self {
                major: 5,
                minor: 7,
                patch: 8,
            }
    }

    /// `utf8mb4` is available from 5.5.3 onwards.
    pub fn supports_utf8mb4(&self) -> bool {
        *self
            >= Self {
                major: 5,
                minor: 5,
                patch: 3,
            }
    }

    /// MySQL 8 defaults to `caching_sha2_password`; older servers must not be
    /// asked for it.
    pub fn default_auth_plugin(&self) -> Option<&'static str> {
        if self.major >= 8 {
            Some("mysql_native_password")
        } else {
            None
        }
    }
}

/// SQL statements for one operation, tagged with the server version that
/// produced them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlPlan {
    pub statements: Vec<String>,
}

impl SqlPlan {
    pub fn new(statements: Vec<String>) -> Self {
        Self { statements }
    }
}

pub fn create_user_sql(
    version: ServerVersion,
    user: &str,
    host: &str,
    password: &str,
) -> Result<SqlPlan> {
    validate_user_name(user)?;
    validate_host(host)?;
    let account = account(user, host);
    let mut statements = Vec::new();
    let guarded = version.supports_create_user_if_not_exists();
    let plugin = version.default_auth_plugin();
    let identified = match plugin {
        Some(plugin) => format!("IDENTIFIED WITH {plugin} BY {}", quote_literal(password)),
        None => format!("IDENTIFIED BY {}", quote_literal(password)),
    };
    if guarded {
        statements.push(format!("CREATE USER IF NOT EXISTS {account} {identified}"));
    } else {
        statements.push(format!("CREATE USER {account} {identified}"));
    }
    Ok(SqlPlan::new(statements))
}

pub fn drop_user_sql(user: &str, host: &str) -> Result<SqlPlan> {
    validate_user_name(user)?;
    validate_host(host)?;
    Ok(SqlPlan::new(vec![format!(
        "DROP USER {}",
        account(user, host)
    )]))
}

pub fn create_database_sql(version: ServerVersion, database: &str) -> Result<SqlPlan> {
    validate_schema_name(database)?;
    let charset = if version.supports_utf8mb4() {
        "utf8mb4"
    } else {
        "utf8"
    };
    Ok(SqlPlan::new(vec![format!(
        "CREATE DATABASE {} CHARACTER SET {charset}",
        quote_ident(database)
    )]))
}

pub fn drop_database_sql(database: &str) -> Result<SqlPlan> {
    validate_schema_name(database)?;
    if is_protected_schema(database) {
        return Err(AppError::usage(format!("拒绝删除系统数据库 `{database}`")));
    }
    Ok(SqlPlan::new(vec![format!(
        "DROP DATABASE {}",
        quote_ident(database)
    )]))
}

/// Grants full privileges on one schema and removes every privilege the
/// account holds anywhere else, so the account can only ever touch that schema.
pub fn bind_sql(database: &str, user: &str, host: &str) -> Result<SqlPlan> {
    validate_schema_name(database)?;
    validate_user_name(user)?;
    validate_host(host)?;
    if is_protected_schema(database) {
        return Err(AppError::usage(format!(
            "拒绝把账号绑定到系统数据库 `{database}`"
        )));
    }
    // Binding revokes global privileges, so applying it to an administrative
    // account would lock the server out of its own administration.
    if is_protected_user(user) {
        return Err(AppError::usage(format!("拒绝绑定受保护的账号 `{user}`")));
    }
    let account = account(user, host);
    let statements = vec![
        // Drop global-level grants first; schema-level revokes below handle the
        // rest. `USAGE` is preserved implicitly because it is not a privilege.
        format!("REVOKE ALL PRIVILEGES, GRANT OPTION FROM {account}"),
        format!(
            "GRANT ALL PRIVILEGES ON {}.* TO {account}",
            quote_ident(database)
        ),
        "FLUSH PRIVILEGES".to_string(),
    ];
    Ok(SqlPlan::new(statements))
}

pub fn unbind_sql(database: &str, user: &str, host: &str) -> Result<SqlPlan> {
    validate_schema_name(database)?;
    validate_user_name(user)?;
    validate_host(host)?;
    if is_protected_user(user) {
        return Err(AppError::usage(format!("拒绝解绑受保护的账号 `{user}`")));
    }
    Ok(SqlPlan::new(vec![
        format!(
            "REVOKE ALL PRIVILEGES ON {}.* FROM {}",
            quote_ident(database),
            account(user, host)
        ),
        "FLUSH PRIVILEGES".to_string(),
    ]))
}

/// Parses the `ON \`db\`.*` portion of a `SHOW GRANTS` line so the caller can
/// spot privileges held on schemas other than the bound one.
///
/// `ON *.*` is a global privilege and yields `None`, because it does not name
/// any single schema.
pub fn schema_from_grant_line(line: &str) -> Option<String> {
    let upper = line.to_ascii_uppercase();
    let on_index = upper.find(" ON ")?;
    let rest = line[on_index + 4..].trim_start();
    if let Some(inner) = rest.strip_prefix('`') {
        let end = inner.find('`')?;
        return Some(inner[..end].to_string());
    }
    let end = rest.find(' ')?;
    let target = rest[..end].trim();
    // `*.*` grants every schema, so it names none of them.
    if target == "*.*" {
        return None;
    }
    Some(target.trim_end_matches(".*").trim_matches('*').to_string())
}

/// Reports whether a grant line grants anything beyond `USAGE`.
pub fn grant_line_confers_privileges(line: &str) -> bool {
    let upper = line.to_ascii_uppercase();
    !upper.starts_with("GRANT USAGE ON")
}

pub const RANDOM_ALPHABET: &[u8] = b"abcdefghijkmnopqrstuvwxyz23456789";
pub const PASSWORD_LEN: usize = 16;

const UPPER: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ";
const LOWER: &[u8] = b"abcdefghijkmnopqrstuvwxyz";
const DIGITS: &[u8] = b"23456789";
const SYMBOLS: &[u8] = b"!@#%^*-_=+";

pub fn random_suffix(len: usize) -> String {
    let mut rng = rand::rng();
    (0..len)
        .map(|_| RANDOM_ALPHABET[rng.random_range(0..RANDOM_ALPHABET.len())] as char)
        .collect()
}

pub fn random_database_name() -> String {
    format!("db_{}", random_suffix(6))
}

pub fn random_user_name() -> String {
    format!("u_{}", random_suffix(6))
}

/// Generates a password that always contains at least one uppercase letter,
/// one lowercase letter, one digit and one symbol, then shuffles the result.
pub fn random_password() -> String {
    let mut rng = rand::rng();
    let pick =
        |rng: &mut rand::rngs::ThreadRng, set: &[u8]| set[rng.random_range(0..set.len())] as char;

    let mut chars: Vec<char> = vec![
        pick(&mut rng, UPPER),
        pick(&mut rng, LOWER),
        pick(&mut rng, DIGITS),
        pick(&mut rng, SYMBOLS),
    ];
    while chars.len() < PASSWORD_LEN {
        chars.push(pick(&mut rng, RANDOM_ALPHABET));
    }

    // Fisher-Yates shuffle so the guaranteed classes are not in fixed slots.
    for index in (1..chars.len()).rev() {
        let swap_with = rng.random_range(0..=index);
        chars.swap(index, swap_with);
    }
    chars.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const V56: ServerVersion = ServerVersion {
        major: 5,
        minor: 6,
        patch: 51,
    };
    const V57: ServerVersion = ServerVersion {
        major: 5,
        minor: 7,
        patch: 44,
    };
    const V80: ServerVersion = ServerVersion {
        major: 8,
        minor: 0,
        patch: 36,
    };

    #[test]
    fn parses_real_version_banners() {
        let banner = r"C:\mysql\bin\mysqld.exe  Ver 5.6.51 for Win64 on x86_64 (MySQL Community Server (GPL))";
        assert_eq!(ServerVersion::parse(banner), Some(V56));
        assert_eq!(
            ServerVersion::parse("Ver 8.0.36-0ubuntu0.22.04.1 for Linux on x86_64"),
            Some(V80)
        );
        assert_eq!(ServerVersion::parse("no digits here"), None);
    }

    #[test]
    fn digits_in_the_executable_path_do_not_override_the_banner_version() {
        // `mysqld --version` prints argv[0] first, so a path like `mysql8` must
        // not be mistaken for the server version.
        let banner = r"D:\mysql8\bin\mysqld.exe  Ver 5.6.51 for Win64 on x86_64";
        assert_eq!(ServerVersion::parse(banner), Some(V56));

        let spaces =
            r"C:\Program Files\MySQL\MySQL Server 5.7\bin\mysqld.exe  Ver 5.7.44 for Win64";
        assert_eq!(
            ServerVersion::parse(spaces),
            Some(ServerVersion {
                major: 5,
                minor: 7,
                patch: 44
            })
        );
    }

    #[test]
    fn a_digit_bearing_path_would_have_chosen_the_wrong_sql_dialect() {
        // Guards the regression directly: parsing this banner must not yield 8.x,
        // which would emit `IDENTIFIED WITH mysql_native_password` on a 5.6 server.
        let version =
            ServerVersion::parse(r"D:\mysql8\bin\mysqld.exe  Ver 5.6.51 for Win64").unwrap();
        assert_eq!(version.major, 5);
        let plan = create_user_sql(version, "app", "localhost", "pw").unwrap();
        assert_eq!(
            plan.statements,
            vec!["CREATE USER 'app'@'localhost' IDENTIFIED BY 'pw'"]
        );
    }

    #[test]
    fn version_capabilities_follow_the_documented_boundaries() {
        assert!(!V56.supports_create_user_if_not_exists());
        assert!(V57.supports_create_user_if_not_exists());
        assert!(V80.supports_create_user_if_not_exists());
        assert!(V56.supports_utf8mb4());
        assert!(!ServerVersion {
            major: 5,
            minor: 5,
            patch: 2
        }
        .supports_utf8mb4());
    }

    #[test]
    fn drop_user_if_exists_is_gated_at_578() {
        // MySQL 5.6 rejects `DROP USER IF EXISTS` with a syntax error, so the
        // capability boundary matters for correctness, not just tidiness.
        assert!(!V56.supports_drop_user_if_exists());
        assert!(!ServerVersion {
            major: 5,
            minor: 7,
            patch: 7
        }
        .supports_drop_user_if_exists());
        assert!(ServerVersion {
            major: 5,
            minor: 7,
            patch: 8
        }
        .supports_drop_user_if_exists());
        assert!(V80.supports_drop_user_if_exists());
    }

    #[test]
    fn create_user_differs_between_56_and_80() {
        let old = create_user_sql(V56, "app", "localhost", "pw").unwrap();
        assert_eq!(
            old.statements,
            vec!["CREATE USER 'app'@'localhost' IDENTIFIED BY 'pw'"]
        );

        let new = create_user_sql(V80, "app", "localhost", "pw").unwrap();
        assert_eq!(
            new.statements,
            vec!["CREATE USER IF NOT EXISTS 'app'@'localhost' IDENTIFIED WITH mysql_native_password BY 'pw'"]
        );
    }

    #[test]
    fn invalid_identifiers_are_rejected_before_reaching_sql() {
        assert!(validate_identifier("user", "app'; DROP DATABASE x; --").is_err());
        assert!(validate_identifier("user", "has space").is_err());
        assert!(validate_identifier("user", "").is_err());
        assert!(validate_identifier("user", &"a".repeat(65)).is_err());
        assert!(validate_identifier("user", "ok_name$1").is_ok());
        assert!(validate_host("local'; DROP").is_err());
    }

    #[test]
    fn password_quotes_are_escaped() {
        assert_eq!(quote_literal(r"a'b\c"), r"'a\'b\\c'");
        let plan = create_user_sql(V56, "app", "localhost", r"p'w\d").unwrap();
        assert!(plan.statements[0].contains(r"IDENTIFIED BY 'p\'w\\d'"));
    }

    #[test]
    fn bind_revokes_global_privileges_then_grants_only_the_target_schema() {
        let plan = bind_sql("appdb", "app", "localhost").unwrap();
        assert_eq!(
            plan.statements,
            vec![
                "REVOKE ALL PRIVILEGES, GRANT OPTION FROM 'app'@'localhost'",
                "GRANT ALL PRIVILEGES ON `appdb`.* TO 'app'@'localhost'",
                "FLUSH PRIVILEGES",
            ]
        );
    }

    #[test]
    fn system_schemas_are_protected() {
        assert!(drop_database_sql("mysql").is_err());
        assert!(drop_database_sql("PERFORMANCE_SCHEMA").is_err());
        assert!(bind_sql("mysql", "app", "localhost").is_err());
        assert!(drop_database_sql("appdb").is_ok());
    }

    #[test]
    fn binding_or_unbinding_a_protected_account_is_refused() {
        // Binding revokes global privileges, so it would strip root's ability to
        // administer the server.
        assert!(bind_sql("appdb", "root", "localhost").is_err());
        assert!(unbind_sql("appdb", "ROOT", "localhost").is_err());
        assert!(bind_sql("appdb", "app", "localhost").is_ok());
    }

    #[test]
    fn database_charset_depends_on_version() {
        assert!(create_database_sql(V56, "appdb").unwrap().statements[0].contains("utf8mb4"));
        assert!(create_database_sql(
            ServerVersion {
                major: 5,
                minor: 1,
                patch: 0
            },
            "appdb"
        )
        .unwrap()
        .statements[0]
            .contains("CHARACTER SET utf8"));
    }

    #[test]
    fn extracts_schema_names_from_grant_lines() {
        assert_eq!(
            schema_from_grant_line("GRANT ALL PRIVILEGES ON `appdb`.* TO 'app'@'localhost'"),
            Some("appdb".to_string())
        );
        // `ON *.*` is a global privilege and names no single schema.
        assert_eq!(
            schema_from_grant_line("GRANT USAGE ON *.* TO 'app'@'localhost'"),
            None
        );
        assert_eq!(
            schema_from_grant_line("GRANT SELECT ON `other`.* TO 'app'@'localhost'"),
            Some("other".to_string())
        );
        assert_eq!(schema_from_grant_line("not a grant"), None);
    }

    #[test]
    fn usage_only_grants_are_not_privileges() {
        assert!(!grant_line_confers_privileges(
            "GRANT USAGE ON *.* TO 'app'@'localhost'"
        ));
        assert!(grant_line_confers_privileges(
            "GRANT ALL PRIVILEGES ON `appdb`.* TO 'app'@'localhost'"
        ));
    }

    #[test]
    fn random_names_have_the_expected_shape() {
        for _ in 0..32 {
            let db = random_database_name();
            assert!(db.starts_with("db_"));
            assert_eq!(db.len(), 9);
            assert!(validate_schema_name(&db).is_ok());

            let user = random_user_name();
            assert!(user.starts_with("u_"));
            assert_eq!(user.len(), 8);
            assert!(validate_user_name(&user).is_ok());

            let password = random_password();
            assert_eq!(password.len(), PASSWORD_LEN);
            assert!(password.chars().any(|ch| ch.is_ascii_uppercase()));
            assert!(password.chars().any(|ch| ch.is_ascii_lowercase()));
            assert!(password.chars().any(|ch| ch.is_ascii_digit()));
            assert!(password.chars().any(|ch| SYMBOLS.contains(&(ch as u8))));
            // Every character must be safe to place in a SQL literal unescaped
            // and safe to pass through an option file.
            assert!(!password.contains('\''));
            assert!(!password.contains('"'));
            assert!(!password.contains('\\'));
        }
    }

    #[test]
    fn protected_users_are_detected_case_insensitively() {
        assert!(is_protected_user("ROOT"));
        assert!(is_protected_user("mysql.sys"));
        assert!(!is_protected_user("app"));
    }
}
