use fs2::FileExt;
use std::fs::{File, OpenOptions};
use std::path::Path;

/// An advisory lock held for the entire lifetime of the LocalFlow process.
pub struct InstanceLock(File);

impl InstanceLock {
    pub fn acquire(path: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(path)?;
        file.try_lock_exclusive()?;
        Ok(Self(file))
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_instance_cannot_acquire_the_lock() {
        let path =
            std::env::temp_dir().join(format!("localflow-instance-test-{}", std::process::id()));
        let first = InstanceLock::acquire(&path).unwrap();
        assert!(InstanceLock::acquire(&path).is_err());
        drop(first);
        let _second = InstanceLock::acquire(&path).unwrap();
        let _ = std::fs::remove_file(path);
    }
}
