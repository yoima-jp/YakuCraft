use std::{env, fs, path::PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let migration_dir = manifest_dir.join("migrations");
    println!("cargo:rerun-if-changed={}", migration_dir.display());

    let mut migrations = fs::read_dir(&migration_dir)
        .expect("Atlas migration directory must exist")
        .map(|entry| entry.expect("migration directory entry").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "sql"))
        .collect::<Vec<_>>();
    migrations.sort_by(|left, right| left.file_name().cmp(&right.file_name()));

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let generated_path = out_dir.join("migrations.rs");
    let mut generated = String::from("pub const MIGRATIONS: &[(&str, &str)] = &[\n");
    for path in migrations {
        println!("cargo:rerun-if-changed={}", path.display());
        let canonical_path = path.canonicalize().expect("migration file path");
        generated.push_str(&format!(
            "    ({:?}, include_str!({:?})),\n",
            path.file_name()
                .and_then(|name| name.to_str())
                .expect("UTF-8 migration filename"),
            canonical_path.to_string_lossy(),
        ));
    }
    generated.push_str("];\n");
    fs::write(generated_path, generated).expect("write embedded migration list");

    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("schema.sql").display()
    );
}
