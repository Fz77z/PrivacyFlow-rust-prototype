#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
mod nonactivating;

#[cfg(target_os = "macos")]
pub use macos::*;
#[cfg(target_os = "macos")]
pub use nonactivating::{make_windows_non_activating, windows_are_non_activating};

#[cfg(not(target_os = "macos"))]
compile_error!("LocalFlow MVP currently supports macOS only.");
