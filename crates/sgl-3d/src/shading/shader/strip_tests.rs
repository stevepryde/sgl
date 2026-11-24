//! Every WGSL file SGL3D and `sgl-post-fx` embed means the same stripped
//! (#895), as a build without debug assertions embeds it
//! (`sgl_post_fx::wgsl`).
use super::tokens::tokens;
use std::path::{Path, PathBuf};

/// The `.wgsl` files under `directory`, recursively.
fn wgsl_files(directory: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            wgsl_files(&path, files);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "wgsl")
        {
            files.push(path);
        }
    }
}

// Plausible defects: stripping drops or joins a token, keeps a comment's
// text, ends a nested or line comment early or late, or drops or merges a
// line so naga's errors move, in a file no frame of the floor tests
// creates (`renderer::floor_tests` compares the modules those create
// through naga). The oracle is the lexer `Scene::add_shader` runs on a
// game's module, written to naga 30's (`tokens`): each file and its
// stripped text have the same tokens, comments skipped, and the same lines.
#[test]
fn every_embedded_wgsl_file_keeps_its_tokens_and_lines_stripped() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for root in [
        crate_root.join("src"),
        crate_root.join("../sgl-post-fx/shaders/wgsl"),
    ] {
        let mut files = Vec::new();
        wgsl_files(&root, &mut files);
        assert!(!files.is_empty(), "no WGSL under {}", root.display());
        for path in files {
            let source = std::fs::read_to_string(&path).unwrap();
            let stripped = sgl_post_fx::wgsl::stripped(&source);
            let name = path.display();
            assert_eq!(stripped.lines().count(), source.lines().count(), "{name}");
            assert!(tokens(&stripped) == tokens(&source), "{name}");
        }
    }
}
