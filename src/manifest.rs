//! Reading and rewriting the version field of project manifests.
//!
//! Every edit touches only the version value itself, so the rest of the file
//! (key order, indentation, comments) stays byte-for-byte the same.

use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

use semver::Version;
use toml_edit::{DocumentMut, Item, Value};

/// A pending change to one manifest, computed before anything is written so a
/// failure in a later file never leaves the earlier ones half-updated.
#[derive(Debug)]
pub struct Plan {
    pub path: PathBuf,
    pub previous: String,
    pub next: String,
    /// `None` when the file is left as it is.
    pub contents: Option<String>,
    pub lock: Option<LockPlan>,
    pub note: Option<String>,
}

#[derive(Debug)]
pub struct LockPlan {
    pub path: PathBuf,
    pub contents: String,
    pub entries: usize,
}

impl Plan {
    pub fn changed(&self) -> bool {
        self.contents.is_some()
    }

    /// Files this plan rewrites: the manifest and, for Cargo, its lock file.
    pub fn written(&self) -> Vec<&Path> {
        let manifest = self.contents.as_ref().map(|_| self.path.as_path());
        let lock = self.lock.as_ref().map(|lock| lock.path.as_path());
        manifest.into_iter().chain(lock).collect()
    }

    pub fn write(&self) -> Result<(), String> {
        if let Some(contents) = &self.contents {
            fs::write(&self.path, contents).map_err(|err| io_error(&self.path, err))?;
        }
        if let Some(lock) = &self.lock {
            fs::write(&lock.path, &lock.contents).map_err(|err| io_error(&lock.path, err))?;
        }
        Ok(())
    }
}

fn io_error(path: &Path, err: std::io::Error) -> String {
    format!("{}: {err}", path.display())
}

#[derive(Clone, Copy)]
enum Kind {
    Json,
    Cargo,
}

fn kind_of(path: &Path) -> Result<Kind, String> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if name == "Cargo.toml" {
        Ok(Kind::Cargo)
    } else if name.ends_with(".json") {
        Ok(Kind::Json)
    } else {
        Err(format!(
            "{}: unsupported file; expected Cargo.toml or a JSON manifest such as package.json",
            path.display()
        ))
    }
}

/// Works out how `path` has to change to carry `next`.
///
/// With `if_newer`, a file that already holds a higher version is left alone,
/// so a hotfix release on an old line never moves the main branch backwards.
pub fn plan(path: &Path, next: &Version, if_newer: bool) -> Result<Plan, String> {
    let kind = kind_of(path)?;
    let text = fs::read_to_string(path).map_err(|err| io_error(path, err))?;
    let in_file = |err: String| format!("{}: {err}", path.display());
    let previous = match kind {
        Kind::Json => json_version(&text),
        Kind::Cargo => parse_toml(&text).and_then(|doc| cargo_version(&doc)),
    }
    .map_err(in_file)?;

    let next_text = next.to_string();
    let mut plan = Plan {
        path: path.to_path_buf(),
        previous: previous.clone(),
        next: next_text.clone(),
        contents: None,
        lock: None,
        note: None,
    };
    if previous == next_text {
        plan.note = Some(format!("already at {next_text}"));
        return Ok(plan);
    }
    match Version::parse(&previous) {
        Ok(current) if if_newer && current > *next => {
            plan.note = Some(format!("at {previous}, newer than {next_text}; left as is"));
            return Ok(plan);
        }
        Err(_) if if_newer => {
            plan.note = Some(format!(
                "\"{previous}\" is not a semantic version; overwriting"
            ));
        }
        _ => {}
    }
    match kind {
        Kind::Json => plan.contents = Some(set_json_version(&text, &next_text).map_err(in_file)?),
        Kind::Cargo => {
            let doc = parse_toml(&text).map_err(in_file)?;
            plan.lock = plan_cargo_lock(path, &doc, &previous, &next_text)?;
            plan.contents = Some(set_cargo_version(doc, &next_text));
        }
    }
    Ok(plan)
}

fn parse_toml(text: &str) -> Result<DocumentMut, String> {
    text.parse()
        .map_err(|err: toml_edit::TomlError| err.to_string())
}

// ---------------------------------------------------------------- JSON

fn json_version(text: &str) -> Result<String, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|err| format!("invalid JSON: {err}"))?;
    let object = value
        .as_object()
        .ok_or("the top level is not a JSON object")?;
    match object.get("version") {
        Some(serde_json::Value::String(version)) => Ok(version.clone()),
        Some(_) => Err("\"version\" is not a string".into()),
        None => Err("no top-level \"version\" field".into()),
    }
}

fn set_json_version(text: &str, next: &str) -> Result<String, String> {
    let span =
        top_level_version_span(text).ok_or("could not locate the top-level \"version\" value")?;
    let quoted = serde_json::to_string(next).map_err(|err| err.to_string())?;
    Ok(format!(
        "{}{quoted}{}",
        &text[..span.start],
        &text[span.end..]
    ))
}

/// Byte range of the top-level `"version"` string value, quotes included.
///
/// A small scanner instead of a JSON round trip: re-serialising would reflow
/// arrays and escapes that tools like npm or prettier laid out on purpose.
fn top_level_version_span(text: &str) -> Option<Range<usize>> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'{' | b'[' => {
                depth += 1;
                index += 1;
            }
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
                index += 1;
            }
            b'"' => {
                let end = string_end(bytes, index)?;
                if depth == 1 {
                    let colon = skip_whitespace(bytes, end);
                    if bytes.get(colon) == Some(&b':') && &text[index + 1..end - 1] == "version" {
                        let value = skip_whitespace(bytes, colon + 1);
                        if bytes.get(value) != Some(&b'"') {
                            return None;
                        }
                        return Some(value..string_end(bytes, value)?);
                    }
                }
                index = end;
            }
            _ => index += 1,
        }
    }
    None
}

/// Index just past the closing quote of the string opening at `start`.
fn string_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut index = start + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            b'"' => return Some(index + 1),
            _ => index += 1,
        }
    }
    None
}

fn skip_whitespace(bytes: &[u8], mut index: usize) -> usize {
    while matches!(bytes.get(index), Some(b' ' | b'\t' | b'\n' | b'\r')) {
        index += 1;
    }
    index
}

// ---------------------------------------------------------------- Cargo

/// Where the version lives: a crate's `[package]`, or a workspace root's
/// `[workspace.package]` that members inherit with `version.workspace = true`.
fn cargo_version_path(doc: &DocumentMut) -> Result<[&'static str; 2], String> {
    if let Some(version) = doc
        .get("package")
        .and_then(|package| package.get("version"))
    {
        return if version.is_str() {
            Ok(["package", "version"])
        } else {
            Err("[package].version is inherited from the workspace; pass the workspace root Cargo.toml instead".into())
        };
    }
    let inherited = doc
        .get("workspace")
        .and_then(|workspace| workspace.get("package"))
        .and_then(|package| package.get("version"));
    match inherited {
        Some(version) if version.is_str() => Ok(["workspace", "package"]),
        Some(_) => Err("[workspace.package].version is not a string".into()),
        None => Err("no [package].version or [workspace.package].version".into()),
    }
}

fn cargo_version_item<'a>(doc: &'a DocumentMut, path: [&str; 2]) -> Option<&'a Item> {
    match path {
        ["package", "version"] => doc.get("package")?.get("version"),
        _ => doc.get("workspace")?.get("package")?.get("version"),
    }
}

fn cargo_version(doc: &DocumentMut) -> Result<String, String> {
    let path = cargo_version_path(doc)?;
    cargo_version_item(doc, path)
        .and_then(Item::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "version is not a string".into())
}

fn set_cargo_version(mut doc: DocumentMut, next: &str) -> String {
    let path = cargo_version_path(&doc).expect("checked while reading the version");
    let item = match path {
        ["package", "version"] => &mut doc["package"]["version"],
        _ => &mut doc["workspace"]["package"]["version"],
    };
    if let Some(value) = item.as_value_mut() {
        let decor = value.decor().clone();
        *value = Value::from(next);
        *value.decor_mut() = decor;
    }
    doc.to_string()
}

/// Keeps Cargo.lock in step with the bumped manifest, otherwise the next
/// `cargo build --locked` fails. Only path packages (no `source`) are touched:
/// the named crate for `[package]`, or every local crate still on the old
/// version for a workspace-wide bump.
fn plan_cargo_lock(
    manifest: &Path,
    doc: &DocumentMut,
    previous: &str,
    next: &str,
) -> Result<Option<LockPlan>, String> {
    let Some(lock_path) = find_cargo_lock(manifest) else {
        return Ok(None);
    };
    let crate_name = match cargo_version_path(doc)? {
        ["package", "version"] => doc
            .get("package")
            .and_then(|package| package.get("name"))
            .and_then(Item::as_str)
            .map(str::to_owned),
        _ => None,
    };
    let lock_text = fs::read_to_string(&lock_path).map_err(|err| io_error(&lock_path, err))?;
    let mut lock =
        parse_toml(&lock_text).map_err(|err| format!("{}: {err}", lock_path.display()))?;
    let mut entries = 0;
    if let Some(packages) = lock
        .get_mut("package")
        .and_then(Item::as_array_of_tables_mut)
    {
        for package in packages.iter_mut() {
            let local = package.get("source").is_none();
            let named = crate_name
                .as_deref()
                .is_none_or(|name| package.get("name").and_then(Item::as_str) == Some(name));
            let outdated = package.get("version").and_then(Item::as_str) == Some(previous);
            if local && named && outdated {
                package["version"] = toml_edit::value(next);
                entries += 1;
            }
        }
    }
    Ok((entries > 0).then(|| LockPlan {
        path: lock_path,
        contents: lock.to_string(),
        entries,
    }))
}

fn find_cargo_lock(manifest: &Path) -> Option<PathBuf> {
    let start = manifest
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()
        .ok()?;
    let lock = start
        .ancestors()
        .map(|dir| dir.join("Cargo.lock"))
        .find(|lock| lock.is_file())?;
    // Relative to the working directory when possible, for readable logs and `git add`.
    let cwd = std::env::current_dir()
        .and_then(|dir| dir.canonicalize())
        .ok();
    Some(
        cwd.and_then(|cwd| lock.strip_prefix(cwd).ok().map(Path::to_path_buf))
            .unwrap_or(lock),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const PACKAGE_JSON: &str = r#"{
  "name": "demo",
  "version": "1.0.0",
  "files": ["dist", "README.md"],
  "dependencies": { "left-pad": "1.0.0", "x": { "version": "9.9.9" } },
  "description": "quote \" and \\ and \"version\": \"0.0.1\""
}
"#;

    #[test]
    fn json_edit_changes_only_the_top_level_value() {
        assert_eq!(json_version(PACKAGE_JSON).unwrap(), "1.0.0");
        let updated = set_json_version(PACKAGE_JSON, "1.2.3").unwrap();
        assert_eq!(
            updated,
            PACKAGE_JSON.replacen("\"version\": \"1.0.0\"", "\"version\": \"1.2.3\"", 1)
        );
        assert_eq!(json_version(&updated).unwrap(), "1.2.3");
    }

    #[test]
    fn json_edit_skips_nested_and_quoted_version_keys() {
        let text = r#"{"engines":{"version":"x"},"note":"\"version\":\"y\"","version" : "0.1.0"}"#;
        assert_eq!(
            set_json_version(text, "2.0.0").unwrap(),
            text.replace("\"0.1.0\"", "\"2.0.0\"")
        );
    }

    #[test]
    fn json_edit_keeps_crlf_and_indentation() {
        let text = "{\r\n\t\"version\":\t\"1.0.0\",\r\n\t\"private\": true\r\n}\r\n";
        assert_eq!(
            set_json_version(text, "1.0.1").unwrap(),
            text.replace("1.0.0", "1.0.1")
        );
    }

    #[test]
    fn json_without_a_string_version_is_rejected() {
        assert!(json_version(r#"{"name":"x"}"#).is_err());
        assert!(json_version(r#"{"version":1}"#).is_err());
        assert!(json_version(r#"["version"]"#).is_err());
    }

    #[test]
    fn cargo_edit_preserves_comments_and_layout() {
        let text = "[package]\nname = \"demo\"\nversion = \"0.1.0\" # bumped by CI\nedition = \"2024\"\n\n[dependencies]\nserde = { version = \"1\" }\n";
        let doc: DocumentMut = text.parse().unwrap();
        assert_eq!(cargo_version(&doc).unwrap(), "0.1.0");
        assert_eq!(
            set_cargo_version(doc, "0.2.0"),
            text.replace("\"0.1.0\"", "\"0.2.0\"")
        );
    }

    #[test]
    fn cargo_workspace_package_version_is_supported() {
        let text = "[workspace]\nmembers = [\"a\"]\n\n[workspace.package]\nversion = \"3.0.0\"\n";
        let doc: DocumentMut = text.parse().unwrap();
        assert_eq!(cargo_version(&doc).unwrap(), "3.0.0");
        assert_eq!(
            set_cargo_version(doc, "3.1.0"),
            text.replace("3.0.0", "3.1.0")
        );
    }

    #[test]
    fn cargo_member_inheriting_the_version_is_rejected() {
        let doc: DocumentMut = "[package]\nname = \"a\"\nversion.workspace = true\n"
            .parse()
            .unwrap();
        assert!(cargo_version(&doc).unwrap_err().contains("workspace root"));
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("release-version-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn plan_respects_if_newer() {
        let dir = scratch("if-newer");
        let path = dir.join("package.json");
        fs::write(&path, "{\n  \"version\": \"2.0.0\"\n}\n").unwrap();

        let older = plan(&path, &Version::new(1, 9, 0), true).unwrap();
        assert!(!older.changed());

        let forced = plan(&path, &Version::new(1, 9, 0), false).unwrap();
        assert_eq!(
            forced.contents.as_deref(),
            Some("{\n  \"version\": \"1.9.0\"\n}\n")
        );

        let same = plan(&path, &Version::new(2, 0, 0), true).unwrap();
        assert!(!same.changed());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn plan_updates_the_crate_entry_in_cargo_lock() {
        let dir = scratch("lock");
        let manifest = dir.join("Cargo.toml");
        fs::write(
            &manifest,
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let lock = "version = 4\n\n[[package]]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[[package]]\nname = \"other\"\nversion = \"0.1.0\"\n\n[[package]]\nname = \"demo\"\nversion = \"0.1.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n";
        fs::write(dir.join("Cargo.lock"), lock).unwrap();

        let plan = plan(&manifest, &Version::new(0, 2, 0), false).unwrap();
        let lock_plan = plan.lock.as_ref().expect("lock is updated");
        assert_eq!(lock_plan.entries, 1);
        assert_eq!(lock_plan.contents, lock.replacen("0.1.0", "0.2.0", 1));

        plan.write().unwrap();
        assert!(
            fs::read_to_string(&manifest)
                .unwrap()
                .contains("version = \"0.2.0\"")
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unsupported_files_are_rejected() {
        assert!(kind_of(Path::new("pyproject.toml")).is_err());
        assert!(kind_of(Path::new("dir/manifest.json")).is_ok());
    }
}
