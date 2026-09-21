#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
mod nonactivating;

#[cfg(target_os = "macos")]
pub use macos::*;
#[cfg(target_os = "macos")]
pub use nonactivating::make_capsule_non_activating;

#[cfg(not(target_os = "macos"))]
compile_error!("LocalFlow MVP currently supports macOS only.");
