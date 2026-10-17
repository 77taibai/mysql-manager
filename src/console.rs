//! Console setup for correct Chinese output on Windows.
//!
//! Rust writes UTF-8 bytes to stdout. A default Chinese Windows console uses
//! code page 936 (GBK), which would render those bytes as mojibake. Switching
//! the console to UTF-8 before printing is the correct fix: it keeps the byte
//! stream valid UTF-8 for pipes and files while making an attached console
//! interpret it correctly.

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    extern "system" {
        fn SetConsoleOutputCP(code_page: u32) -> i32;
        fn SetConsoleCP(code_page: u32) -> i32;
        fn GetConsoleWindow() -> *mut c_void;
    }

    const CP_UTF8: u32 = 65001;

    /// Switches the attached console to UTF-8. Does nothing when output is
    /// redirected, where the byte stream is already correct as UTF-8.
    pub fn use_utf8() {
        // SAFETY: these Win32 calls only read/modify console state of the
        // current process and have no memory-safety preconditions.
        unsafe {
            if GetConsoleWindow().is_null() {
                return;
            }
            SetConsoleOutputCP(CP_UTF8);
            SetConsoleCP(CP_UTF8);
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn use_utf8() {}
}

pub use imp::use_utf8;
