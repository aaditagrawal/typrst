//! Package & path resolution for `#import` / `#include`.
//!
//! `"@ns/name:ver"` specs look in the typst package caches
//! (`$XDG_DATA_HOME/typst/packages`, `$XDG_CACHE_HOME/typst/packages`,
//! then `~/.cache/typrst/packages`); `@preview` packages are fetched
//! from packages.typst.org with `curl` and unpacked with `tar` — both
//! std-only via `std::process::Command`. Plain path specs resolve
//! relative to the importing file's directory.
use crate::ir::{Diag, Node};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

/// Resolve an import spec to the `.typ` file to load.
///
/// `spec` is either `"@ns/name:ver"` (a package) or a filesystem path
/// relative to `base_dir`. For a package the result is the manifest's
/// `entrypoint`; for a path it is the canonicalized file.
pub fn resolve(spec: &str, base_dir: &Path) -> Result<PathBuf, String> {
    if let Some(rest) = spec.strip_prefix('@') {
        resolve_package(rest)
    } else {
        base_dir
            .join(spec)
            .canonicalize()
            .map_err(|e| format!("cannot resolve '{spec}': {e}"))
    }
}

/// `ns/name:ver` -> (ns, name, ver).
fn parse_spec(rest: &str) -> Result<(&str, &str, &str), String> {
    let (ns, nv) = rest
        .split_once('/')
        .ok_or_else(|| format!("malformed package spec '@{rest}' (expected @ns/name:ver)"))?;
    let (name, ver) = nv
        .rsplit_once(':')
        .ok_or_else(|| format!("malformed package spec '@{rest}' (expected name:version)"))?;
    if ns.is_empty() || name.is_empty() || ver.is_empty() {
        return Err(format!("malformed package spec '@{rest}'"));
    }
    Ok((ns, name, ver))
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Package roots in search order: XDG data, XDG cache, typrst cache.
fn roots() -> Vec<PathBuf> {
    let mut v = Vec::new();
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|h| h.join(".local/share")));
    if let Some(d) = data {
        v.push(d.join("typst/packages"));
    }
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|h| h.join(".cache")));
    if let Some(c) = cache {
        v.push(c.join("typst/packages"));
    }
    if let Some(h) = home() {
        v.push(h.join(".cache/typrst/packages"));
    }
    v
}

/// typrst's own download cache: `~/.cache/typrst/packages`.
fn typrst_cache() -> Option<PathBuf> {
    home().map(|h| h.join(".cache/typrst/packages"))
}

fn resolve_package(rest: &str) -> Result<PathBuf, String> {
    let (ns, name, ver) = parse_spec(rest)?;
    for r in roots() {
        let dir = r.join(ns).join(name).join(ver);
        if dir.is_dir() {
            return entrypoint(&dir, name, ver);
        }
    }
    if ns != "preview" {
        return Err(format!(
            "package '@{rest}' not found (only @preview packages are downloadable)"
        ));
    }
    let dir = download(ns, name, ver)?;
    entrypoint(&dir, name, ver)
}

/// Fetch `https://packages.typst.org/<ns>/<name>-<ver>.tar.gz` into the
/// typrst cache via curl + tar.
fn download(ns: &str, name: &str, ver: &str) -> Result<PathBuf, String> {
    let dest = typrst_cache()
        .ok_or("no HOME directory for package cache")?
        .join(ns)
        .join(name)
        .join(ver);
    std::fs::create_dir_all(&dest).map_err(|e| format!("cannot create {}: {e}", dest.display()))?;
    let tmp = dest.join(".download.tar.gz");
    let url = format!("https://packages.typst.org/{ns}/{name}-{ver}.tar.gz");
    let ok = Command::new("curl")
        .args(["-fsSL", "-o"])
        .arg(&tmp)
        .arg(&url)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!(
            "download of '@{ns}/{name}:{ver}' failed (curl missing or HTTP error)"
        ));
    }
    let ok = Command::new("tar")
        .arg("-xzf")
        .arg(&tmp)
        .arg("-C")
        .arg(&dest)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let _ = std::fs::remove_file(&tmp);
    if !ok {
        return Err(format!("failed to unpack '@{ns}/{name}:{ver}'"));
    }
    Ok(dest)
}

/// Read `typst.toml` in `dir`, sanity-check name/version, and return
/// `dir.join(entrypoint)`.
fn entrypoint(dir: &Path, name: &str, ver: &str) -> Result<PathBuf, String> {
    let manifest = dir.join("typst.toml");
    let src = std::fs::read_to_string(&manifest)
        .map_err(|e| format!("package at {} has no typst.toml: {e}", dir.display()))?;
    let t = parse_toml(&src);
    if let Some(n) = t.get("name") {
        if n != name {
            return Err(format!(
                "package name mismatch: spec '{name}', manifest '{n}'"
            ));
        }
    }
    if let Some(v) = t.get("version") {
        if v != ver {
            return Err(format!(
                "package version mismatch: spec '{ver}', manifest '{v}'"
            ));
        }
    }
    let ep = t
        .get("entrypoint")
        .ok_or_else(|| format!("typst.toml in {} has no package entrypoint", dir.display()))?;
    let f = dir.join(ep);
    if f.is_file() {
        Ok(f)
    } else {
        Err(format!("entrypoint '{ep}' not found in {}", dir.display()))
    }
}

/// Minimal TOML: `[package]` section, `key = "value"` string pairs.
fn parse_toml(src: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    let mut in_pkg = false;
    for line in src.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_pkg = line == "[package]";
            continue;
        }
        if !in_pkg {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim().trim_matches('"');
        if let Some(v) = v.trim().strip_prefix('"').and_then(|v| v.split('"').next()) {
            m.insert(k.to_string(), v.to_string());
        }
    }
    m
}

thread_local! {
    /// Parsed-AST cache so importing the same file twice doesn't re-parse.
    static MODULES: RefCell<HashMap<PathBuf, Rc<Vec<Node>>>> = RefCell::new(HashMap::new());
}

/// Read and parse a `.typ` file. Results are cached by path, so a hit
/// returns an empty diag list (its diagnostics were reported on first
/// load). `path` should be canonicalized by the caller.
pub fn load(path: &Path) -> Result<(Rc<Vec<Node>>, Vec<Diag>), String> {
    if let Some(n) = MODULES.with(|m| m.borrow().get(path).cloned()) {
        return Ok((n, Vec::new()));
    }
    let src = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let (nodes, diags) = crate::syntax::parse(&src);
    let nodes = Rc::new(nodes);
    MODULES.with(|m| m.borrow_mut().insert(path.to_path_buf(), nodes.clone()));
    Ok((nodes, diags))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_parse() {
        assert_eq!(
            parse_spec("preview/cetz:0.3.1"),
            Ok(("preview", "cetz", "0.3.1"))
        );
        assert_eq!(parse_spec("x/a-b:1"), Ok(("x", "a-b", "1")));
        assert!(parse_spec("nonsense").is_err());
        assert!(parse_spec("ns/nover").is_err());
        assert!(parse_spec("/name:1").is_err());
        assert!(parse_spec("ns/:1").is_err());
    }

    #[test]
    fn toml_parse() {
        let src = "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nentrypoint = \"src/lib.typ\"\n\n[template]\nx = \"y\"\n";
        let t = parse_toml(src);
        assert_eq!(t.get("name").map(String::as_str), Some("demo"));
        assert_eq!(t.get("version").map(String::as_str), Some("0.1.0"));
        assert_eq!(t.get("entrypoint").map(String::as_str), Some("src/lib.typ"));
        assert!(!t.contains_key("x")); // outside [package]
    }

    #[test]
    fn resolve_path_spec() {
        let dir = std::env::temp_dir().join(format!("typrst-pkg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("m.typ"), "#let x = 1\n").unwrap();
        assert_eq!(
            resolve("m.typ", &dir).unwrap(),
            dir.join("m.typ").canonicalize().unwrap()
        );
        assert!(resolve("missing.typ", &dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_package_errors() {
        // Non-preview namespaces are never downloaded.
        let e = resolve("@other/zz-nonexistent:9.9.9", Path::new(".")).unwrap_err();
        assert!(e.contains("not found") || e.contains("typst.toml"), "{e}");
        assert!(resolve("@bad", Path::new(".")).is_err());
    }
}
