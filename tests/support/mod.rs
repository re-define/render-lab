pub fn tempdir() -> std::io::Result<tempfile::TempDir> {
    let root = std::path::Path::new("target/test-data");
    std::fs::create_dir_all(root)?;
    tempfile::tempdir_in(root)
}
