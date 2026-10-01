use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT: AtomicU64 = AtomicU64::new(0);

pub struct TestDir {
    pub path: PathBuf,
    root: PathBuf,
}
impl TestDir {
    pub fn new(name: &str) -> Self {
        let root = PathBuf::from(
            std::env::var_os("RECUVORA_TEST_TEMP").expect("external test root required"),
        );
        assert!(root.is_absolute());
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        assert!(
            !root.starts_with(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .canonicalize()
                    .unwrap()
            )
        );
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = root.join(format!(
            "approval-flow-{name}-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self { path, root }
    }
}
impl Drop for TestDir {
    fn drop(&mut self) {
        if self.path.parent() == Some(self.root.as_path()) && self.path.starts_with(&self.root) {
            let result = std::fs::remove_dir_all(&self.path);
            if !std::thread::panicking() {
                result.unwrap();
            }
        }
    }
}
