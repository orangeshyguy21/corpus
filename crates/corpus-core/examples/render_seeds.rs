//! Regenerate the checked-in `.opencode/agent/{operator,researcher}.md`
//! from the seeds in `store/templates/agents/` — mirrors exactly what the
//! `templates` test asserts byte-for-byte (temp store, project `default`,
//! render, copy out). Run after editing seed prompts or the renderer:
//!
//! ```sh
//! cargo run -p corpus-core --example render_seeds
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use corpus_core::Store;

fn copy_tree(src: &Path, dst: &Path) {
    if !src.is_dir() {
        fs::create_dir_all(dst.parent().unwrap()).unwrap();
        fs::copy(src, dst).unwrap();
        return;
    }
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap().flatten() {
        copy_tree(&entry.path(), &dst.join(entry.file_name()));
    }
}

fn main() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let seed_src = repo.join("store/templates/agents");

    let tmp = std::env::temp_dir().join(format!("corpus-render-seeds-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    let store = Store::new(tmp.clone());
    copy_tree(&seed_src, &store.seed_agents_dir());
    store
        .create_project("default", "Default corpus project", "cdk-regtest")
        .expect("create default project");

    let out_dir = repo.join(".opencode").join("agent");
    for slug in corpus_core::CORE_SEEDS {
        let written = store.render_agent("default", slug).expect("render agent");
        for path in written {
            let dest = out_dir.join(path.file_name().unwrap());
            fs::copy(&path, &dest).expect("copy into .opencode/agent");
            println!("wrote {}", dest.display());
        }
    }
    let _ = fs::remove_dir_all(&tmp);
}
