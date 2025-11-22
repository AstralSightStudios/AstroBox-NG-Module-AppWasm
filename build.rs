use std::env;
use std::path::PathBuf;

fn main() {
    emit_priv_cfg_flag();
    println!("cargo::rustc-check-cfg=cfg(rust_analyzer)");
}

fn emit_priv_cfg_flag() {
    if let Some(marker) = find_existing_marker() {
        println!("cargo:rerun-if-changed={}", marker.display());
        println!("cargo:rustc-cfg=astrobox_priv_cloned");
    } else {
        println!("cargo:rerun-if-env-changed=ASTROBOX_PRIV_CLONED");
    }
}

fn find_existing_marker() -> Option<PathBuf> {
    priv_marker_candidates()
        .into_iter()
        .find(|candidate| candidate.exists())
}

fn priv_marker_candidates() -> Vec<PathBuf> {
    let mut markers = Vec::new();
    let mut current = workspace_dir();
    for _ in 0..4 {
        let Some(dir) = current.clone() else {
            break;
        };
        markers.push(dir.join("__PRIV_CLONED"));
        current = dir.parent().map(|p| p.to_path_buf());
    }
    markers
}

fn workspace_dir() -> Option<PathBuf> {
    if let Ok(dir) = env::var("CARGO_WORKSPACE_DIR") {
        return Some(PathBuf::from(dir));
    }
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").ok()?);
    manifest_dir.ancestors().nth(2).map(|p| p.to_path_buf())
}
