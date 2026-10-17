use std::fmt;

pub const EXIT_GENERAL: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_INSTANCE: i32 = 3;
pub const EXIT_MYSQL: i32 = 4;

#[derive(Debug)]
pub struct AppError {
    code: i32,
    message: String,
}

impl AppError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn general(message: impl Into<String>) -> Self {
        Self::new(EXIT_GENERAL, message)
    }

    pub fn usage(message: impl Into<String>) -> Self {
        Self::new(EXIT_USAGE, message)
    }

    pub fn instance(message: impl Into<String>) -> Self {
        Self::new(EXIT_INSTANCE, message)
    }

    pub fn mysql(message: impl Into<String>) -> Self {
        Self::new(EXIT_MYSQL, message)
    }

    pub fn code(&self) -> i32 {
        self.code
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AppError {}

pub type Result<T> = std::result::Result<T, AppError>;
