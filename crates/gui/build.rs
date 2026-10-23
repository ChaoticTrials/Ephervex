use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = slint_build::CompilerConfiguration::new()
        // Plain `fluent`, not `fluent-dark`: the suffix folds the platform's
        // color scheme to a constant, and the window writes the widget
        // palette's scheme itself from the one the user chose.
        .with_style("fluent".into())
        // Slint's element-query API, which the pointer-target tests use to
        // click a real button in a real window, can only find an element when
        // the compiler kept its debug info. Release builds do not need it and
        // should not carry it, so it is kept for every other profile.
        .with_debug_info(std::env::var("PROFILE").as_deref() != Ok("release"));
    slint_build::compile_with_config("ui/app.slint", config)?;
    write_ui_sources()
}

/// Writes the list of every `.slint` module under `ui/` as `(path, source)`
/// pairs for the tests that read the markup as text.
fn write_ui_sources() -> Result<(), Box<dyn std::error::Error>> {
    // A module added or removed changes the directory, which reruns this script.
    println!("cargo:rerun-if-changed=ui");
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let root = manifest.join("ui");
    let mut modules = Vec::new();
    collect_modules(&root, &root, &mut modules)?;
    modules.sort();

    let mut list = String::from("&[\n");
    for relative in &modules {
        let absolute = root.join(relative);
        writeln!(
            list,
            "    ({relative:?}, include_str!({:?})),",
            absolute.to_string_lossy()
        )?;
    }
    list.push_str("]\n");
    let out = PathBuf::from(std::env::var("OUT_DIR")?).join("ui_sources.rs");
    std::fs::write(out, list)?;
    Ok(())
}

fn collect_modules(
    directory: &Path,
    root: &Path,
    found: &mut Vec<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_modules(&path, root, found)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "slint")
        {
            let relative = path.strip_prefix(root)?;
            let parts: Vec<_> = relative
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .collect();
            found.push(parts.join("/"));
        }
    }
    Ok(())
}
