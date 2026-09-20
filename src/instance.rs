use anyhow::{Context, Result};
use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;

/// An advisory lock held for the entire lifetime of the LocalFlow process.
/// A second instance would install a second HID event tap and race the first
/// one to paste into whatever application happens to be focused.
pub struct InstanceLock {
    /// Held open purely for its lock side effect. macOS releases the lock when
    /// this file closes, including when the process exits abnormally.
    _locked_file: File,
}

impl InstanceLock {
    /// Take the exclusive lock, distinguishing "another instance holds it" from
    /// a genuine failure to open the lock file.
    pub fn acquire(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .with_context(|| format!("Could not open the lock file at {}", path.display()))?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _locked_file: file }),
            Err(TryLockError::WouldBlock) => Err(anyhow::anyhow!(
                "another instance is already running, and a second input listener \
                 would race it to paste into the focused application"
            )),
            Err(TryLockError::Error(error)) => Err(error)
                .with_context(|| format!("Could not lock the lock file at {}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A held lock must exclude a second instance, because two instances would
    /// install two HID event taps and race each other to paste.
    ///
    /// Release is deliberately not asserted here. It is a property of the
    /// operating system's advisory lock, which ends when the last descriptor
    /// for the open file closes, including on an abnormal exit. Asserting it
    /// in-process is flaky, because any other test that spawns a child
    /// transiently inherits this descriptor across the fork.
    #[test]
    fn second_instance_cannot_acquire_the_lock() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "localflow-instance-test-{}-{unique}",
            std::process::id()
        ));
        let _first = InstanceLock::acquire(&path).unwrap();
        assert!(InstanceLock::acquire(&path).is_err());
        let _ = std::fs::remove_file(path);
    }
}
