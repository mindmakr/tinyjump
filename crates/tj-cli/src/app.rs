//! Reads an app: its entry file and every file it imports, as one bundle (see
//! `tj_core::modules`). The core does no I/O, so following imports happens here.

use std::path::{Path, PathBuf};
use tj_core::modules;

#[derive(Debug)]
pub struct App {
    /// The bundle; a one-file app is just its source.
    pub source: String,
    /// Every file the app reads or imports, to watch for changes.
    pub files: Vec<PathBuf>,
}

pub fn read(entry: &Path) -> Result<App, String> {
    // Precompiled binary `.tjb` bytecode: wrap into the UTF-8 bundle format so every command
    // (`tj serve`, `tj bench`, `tj reload`) loads it without invoking the compiler.
    if let Ok(raw) = std::fs::read(entry) {
        if raw.starts_with(tj_core::vm::bytecode::MAGIC) {
            return Ok(App { source: tj_core::vm::bytecode::bundle_from_bytes(&raw), files: vec![entry.to_path_buf()] });
        }
    }
    let dir = entry.parent().unwrap_or(Path::new(""));
    let entry_name = entry.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or_else(|| format!("{} is not a file", entry.display()))?;
    let mut names = vec![entry_name];
    let mut sources = Vec::new();
    let mut watched: Vec<PathBuf> = Vec::new();
    let mut lock_watched = false;
    let mut i = 0;
    while i < names.len() {
        // Built-in plugin modules (`tj:ai`) come from `tj-core`. Other `tj:<name>` imports
        // resolve from `tj.lock` + `.tj-packages/<name>/plugin.tj` after SHA-256 verification.
        if let Some(pkg_name) = names[i].strip_prefix("tj:") {
            if !modules::BUILT_IN.iter().any(|(b, _)| *b == names[i]) {
                if let Some((src, file)) = crate::registry::load_locked_module(dir, pkg_name)? {
                    if let Ok(program) = tj_core::syntax::parse(&src) {
                        for imp in program.imports {
                            if !imp.from.starts_with("tj:") {
                                return Err(format!("locked package `{}` cannot import local file {:?}", names[i], imp.from));
                            }
                            if let Ok(name) = modules::resolve(&names[i], &imp.from) {
                                if !names.contains(&name) {
                                    names.push(name);
                                }
                            }
                        }
                    }
                    sources.push((names[i].clone(), src));
                    watched.push(file);
                    if !lock_watched {
                        watched.push(dir.join(crate::registry::LOCKFILE_NAME));
                        lock_watched = true;
                    }
                }
            }
            i += 1;
            continue;
        }
        let path = dir.join(&names[i]);
        watched.push(path.clone());
        match std::fs::read_to_string(&path) {
            Ok(src) => {
                // Unparsable files still go in: compiling the bundle reports their errors.
                if let Ok(program) = tj_core::syntax::parse(&src) {
                    for imp in program.imports {
                        if let Ok(name) = modules::resolve(&names[i], &imp.from) {
                            if !names.contains(&name) {
                                names.push(name);
                            }
                        }
                    }
                }
                sources.push((names[i].clone(), src));
            }
            // A missing import is reported by the compiler, with a hint.
            Err(_) if i > 0 => {}
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        }
        i += 1;
    }
    Ok(App { source: modules::bundle(&sources), files: watched })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_imports_from_the_entry() {
        let dir = std::env::temp_dir().join(format!("tj-app-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("lib")).expect("temp dir");
        std::fs::write(dir.join("app.tj"), "import { a } from \"./lib/a\"\nimport { gone } from \"./gone\"\n").expect("write");
        std::fs::write(dir.join("lib/a.tj"), "import { b } from \"../b\"\nexport const a = 1\n").expect("write");
        std::fs::write(dir.join("b.tj"), "export const b = 2\n").expect("write");
        std::fs::write(dir.join("unused.tj"), "const nobody = 0\n").expect("write");
        let app = read(&dir.join("app.tj")).expect("reads");
        let names: Vec<String> = modules::files(&app.source).into_iter().map(|f| f.name).collect();
        assert_eq!(names, ["app.tj", "lib/a.tj", "b.tj"], "imported files only; a missing one is left to the compiler");
        assert_eq!(app.files.len(), 4, "watches the missing file too, so creating it reloads");
        let one = read(&dir.join("b.tj")).expect("reads");
        assert_eq!(one.source, "export const b = 2\n", "a one-file app is its source");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
