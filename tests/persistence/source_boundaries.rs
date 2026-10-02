use std::{fs, path::Path};

pub fn assert_rust_tree_excludes(relative_root: &str, forbidden: &[&str]) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative_root);
    let mut pending = vec![root];
    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
        {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
                continue;
            }
            let source = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
            for pattern in forbidden {
                assert!(
                    !source.contains(pattern),
                    "{} contains forbidden architecture dependency {pattern:?}",
                    path.display()
                );
            }
        }
    }
}

#[allow(dead_code)]
pub fn rust_source(relative_path: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative_path);
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
}
