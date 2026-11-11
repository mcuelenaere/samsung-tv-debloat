//! Every reviewed suite component spec must build from a flat artifact
//! directory, and the fixed order must name those components.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn specs() -> Option<Vec<PathBuf>> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../rooting/packages");
    if !dir.is_dir() {
        return None; // sparse checkout: rooting/ is intentionally absent
    }
    let mut specs: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    specs.sort();
    Some(specs)
}

#[test]
fn every_suite_component_spec_builds() {
    let Some(specs) = specs() else {
        eprintln!("skipping: rooting/packages is absent");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    fs::create_dir_all(&source).unwrap();
    let mut ids = BTreeSet::new();

    for spec_path in specs {
        let spec: serde_json::Value =
            serde_json::from_slice(&fs::read(&spec_path).unwrap()).unwrap();
        let id = spec["id"].as_str().unwrap().to_owned();
        for key in ["files", "units"] {
            if let Some(entries) = spec[key].as_array() {
                for entry in entries {
                    let name = entry["source"].as_str().unwrap();
                    fs::write(source.join(name), format!("{id}:{name}")).unwrap();
                }
            }
        }
        if let Some(package) = spec.get("package") {
            let name = package["source"].as_str().unwrap();
            fs::write(source.join(name), format!("{id}:package")).unwrap();
        }
        let output = dir.path().join(format!("{id}.tvpkg"));
        tv_bundle::build_package(&spec_path, &source, &output).unwrap();
        assert!(output.is_file(), "{id}");
        ids.insert(id);
    }

    let order = tv_bundle::read_order(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../rooting/packages/package-order.txt"),
    )
    .unwrap();
    let ordered: BTreeSet<String> = order.into_iter().map(|(id, _)| id).collect();
    assert_eq!(ordered, ids);
}
