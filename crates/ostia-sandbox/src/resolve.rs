use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Resolved binary with all its dependencies.
#[derive(Debug)]
pub struct ResolvedBinary {
    pub path: PathBuf,
    pub interpreter: Option<PathBuf>,
    pub libraries: HashSet<PathBuf>,
}

/// Resolve a binary name to its full path using `which`.
pub fn which(binary: &str) -> Result<PathBuf> {
    let output = Command::new("which")
        .arg(binary)
        .output()
        .with_context(|| format!("failed to run 'which {}'", binary))?;

    if !output.status.success() {
        anyhow::bail!("binary '{}' not found on host system", binary);
    }

    let path = String::from_utf8(output.stdout)
        .context("invalid UTF-8 in which output")?
        .trim()
        .to_string();

    // Resolve symlinks to get the real path
    let resolved = std::fs::canonicalize(&path)
        .with_context(|| format!("failed to resolve symlink for {}", path))?;

    Ok(resolved)
}

/// Parse an ELF binary and extract its dynamic library dependencies.
pub fn resolve_binary_deps(binary_path: &Path) -> Result<ResolvedBinary> {
    let bytes = std::fs::read(binary_path)
        .with_context(|| format!("failed to read binary: {}", binary_path.display()))?;

    let elf = goblin::elf::Elf::parse(&bytes)
        .with_context(|| format!("failed to parse ELF: {}", binary_path.display()))?;

    let interpreter = elf.interpreter.map(PathBuf::from);

    // Collect all needed sonames
    let sonames: Vec<String> = elf.libraries.iter().map(|s| s.to_string()).collect();

    // Resolve sonames to absolute paths
    let lib_paths = resolve_sonames(&sonames, &elf.runpaths, binary_path)?;

    // Recursively resolve library dependencies
    let mut all_libs = HashSet::new();
    let mut queue: Vec<PathBuf> = lib_paths.into_iter().collect();
    let mut seen = HashSet::new();

    while let Some(lib_path) = queue.pop() {
        if seen.contains(&lib_path) {
            continue;
        }
        seen.insert(lib_path.clone());

        // Resolve symlinks
        let real_path = std::fs::canonicalize(&lib_path).unwrap_or(lib_path.clone());
        all_libs.insert(real_path.clone());

        // If the symlink and real path differ, we need both
        if lib_path != real_path {
            all_libs.insert(lib_path.clone());
        }

        // Parse this library for its own dependencies
        if let Ok(bytes) = std::fs::read(&real_path) {
            if let Ok(elf) = goblin::elf::Elf::parse(&bytes) {
                let child_sonames: Vec<String> =
                    elf.libraries.iter().map(|s| s.to_string()).collect();
                if let Ok(child_paths) = resolve_sonames(&child_sonames, &elf.runpaths, &real_path)
                {
                    for p in child_paths {
                        if !seen.contains(&p) {
                            queue.push(p);
                        }
                    }
                }
            }
        }
    }

    // Resolve the interpreter's real path, keeping both raw and canonical.
    // The kernel uses the raw ELF header path to find the interpreter, so
    // it must be mountable at that location. The canonical path is stored
    // as the interpreter field; the raw path is added to libraries so it
    // also gets bind-mounted.
    let interpreter = interpreter.and_then(|i| {
        let raw = PathBuf::from(&i);
        if raw.exists() {
            let canonical = std::fs::canonicalize(&raw).ok();
            if let Some(ref cp) = canonical {
                if *cp != raw {
                    all_libs.insert(raw.clone());
                }
            }
            canonical.or(Some(raw))
        } else {
            Some(raw)
        }
    });

    Ok(ResolvedBinary {
        path: binary_path.to_path_buf(),
        interpreter,
        libraries: all_libs,
    })
}

/// Resolve a list of sonames to absolute paths using ldconfig cache.
fn resolve_sonames(
    sonames: &[String],
    runpaths: &[&str],
    binary_path: &Path,
) -> Result<Vec<PathBuf>> {
    let cache = load_ldconfig_cache()?;
    let binary_dir = binary_path.parent().unwrap_or(Path::new("/"));

    let mut resolved = Vec::new();

    for soname in sonames {
        // 1. Check runpaths (with $ORIGIN expansion)
        let mut found = false;
        for rp in runpaths {
            let expanded = rp.replace("$ORIGIN", &binary_dir.to_string_lossy());
            let candidate = PathBuf::from(&expanded).join(soname);
            if candidate.exists() {
                resolved.push(candidate);
                found = true;
                break;
            }
        }
        if found {
            continue;
        }

        // 2. Check ldconfig cache
        if let Some(path) = cache.get(soname.as_str()) {
            resolved.push(path.clone());
            continue;
        }

        // 3. Check default paths
        let default_paths = ["/lib", "/lib64", "/usr/lib", "/usr/lib64", "/usr/local/lib"];
        let mut found_default = false;
        for dir in &default_paths {
            let candidate = PathBuf::from(dir).join(soname);
            if candidate.exists() {
                resolved.push(candidate);
                found_default = true;
                break;
            }
        }
        if !found_default {
            // Not fatal — some libs might be optional or already linked statically
            eprintln!("warning: could not resolve library: {}", soname);
        }
    }

    Ok(resolved)
}

/// Parse ldconfig -p output to build a soname -> path cache.
fn load_ldconfig_cache() -> Result<HashMap<String, PathBuf>> {
    let output = Command::new("ldconfig")
        .arg("-p")
        .output()
        .context("failed to run ldconfig -p")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut cache = HashMap::new();

    for line in stdout.lines().skip(1) {
        // Format: "	libz.so.1 (libc6,x86-64) => /lib/x86_64-linux-gnu/libz.so.1"
        let line = line.trim();
        if let Some(arrow_pos) = line.find("=>") {
            let soname = line[..line.find(" (").unwrap_or(arrow_pos)]
                .trim()
                .to_string();
            let path = line[arrow_pos + 2..].trim().to_string();
            cache.insert(soname, PathBuf::from(path));
        }
    }

    Ok(cache)
}

/// Resolve all binaries in a profile and return the full dependency tree.
pub fn resolve_profile_binaries(
    binaries: &std::collections::HashSet<String>,
) -> HashMap<String, Result<ResolvedBinary>> {
    let mut results = HashMap::new();

    for binary in binaries {
        let result = which(binary).and_then(|path| resolve_binary_deps(&path));
        results.insert(binary.clone(), result);
    }

    results
}
