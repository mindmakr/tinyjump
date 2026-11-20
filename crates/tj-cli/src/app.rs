//! Reads an app: its entry file and every file it imports, as one bundle (see
//! `tj_core::modules`). The core does no I/O, so following imports happens here.

use std::path::{Path, PathBuf};
use tj_core::modules;

pub struct App {
    /// The bundle; a one-file app is just its source.
    pub source: String,
    /// Every file the app reads or imports, to watch for changes.
    pub files: Vec<PathBuf>,
}

pub fn read(entry: &Path) -> Result<App, String> {
    let dir = entry.parent().unwrap_or(Path::new(""));
    let entry_name = entry.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or_else(|| format!("{} is not a file", entry.display()))?;
    let mut names = vec![entry_name];
    let mut sources = Vec::new();
    let mut i = 0;
    while i < names.len() {
        let path = dir.join(&names[i]);
        // Plugin modules (`tj:ai`) are not files: the compiler adds them.
        if names[i].starts_with("tj:") {
            i += 1;
            continue;
        }
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
    Ok(App { source: modules::bundle(&sources), files: names.iter().filter(|n| !n.starts_with("tj:")).map(|n| dir.join(n)).collect() })
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
