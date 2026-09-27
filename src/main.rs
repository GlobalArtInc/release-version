mod manifest;
mod version;

use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
Set the project version from a release tag.

Usage: release-version <TAG> [--file <PATH>]... [--if-newer] [--strict]

Arguments:
  <TAG>          v1.2.3, 1.2.3 or refs/tags/v1.2.3

Options:
  --file <PATH>  Manifest to update: Cargo.toml or a JSON file with a top-level
                 \"version\" (package.json, manifest.json). Repeatable. Defaults to
                 package.json, then Cargo.toml, in the current directory.
  --if-newer     Never move a version backwards (hotfix tags on an old line)
  --strict       Allow only plain X.Y.Z with parts up to 65535, as browser
                 extension stores require
  -h, --help     Print this help
  -V, --version  Print the tool version

Updating a Cargo.toml also updates the matching entries in Cargo.lock.
Inside GitHub Actions, `version`, `previous`, `changed`, `prerelease` and
`files` (every file written, one per line) are appended to $GITHUB_OUTPUT.";

#[derive(Debug, Default)]
struct Args {
    tag: String,
    files: Vec<PathBuf>,
    if_newer: bool,
    strict: bool,
}

enum Command {
    Run(Args),
    Help,
    Version,
}

fn parse_args(raw: impl IntoIterator<Item = String>) -> Result<Command, String> {
    let mut args = Args::default();
    let mut tag = None;
    let mut raw = raw.into_iter();
    while let Some(arg) = raw.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "-V" | "--version" => return Ok(Command::Version),
            "--if-newer" => args.if_newer = true,
            "--strict" => args.strict = true,
            "--file" => {
                let path = raw.next().ok_or("--file needs a path")?;
                args.files.push(PathBuf::from(path));
            }
            _ if arg.starts_with("--file=") => {
                args.files.push(PathBuf::from(&arg["--file=".len()..]))
            }
            _ if arg.starts_with('-') && arg.len() > 1 => {
                return Err(format!("unknown option {arg}"));
            }
            _ if tag.is_none() => tag = Some(arg),
            _ => return Err(format!("unexpected argument {arg}")),
        }
    }
    args.tag = tag.ok_or("missing <TAG>")?;
    Ok(Command::Run(args))
}

fn default_files() -> Result<Vec<PathBuf>, String> {
    ["package.json", "Cargo.toml"]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
        .map(|path| vec![path])
        .ok_or_else(|| "no package.json or Cargo.toml in the current directory; pass --file".into())
}

fn run(args: Args) -> Result<(), String> {
    let next = version::parse_tag(&args.tag, args.strict)?;
    let files = if args.files.is_empty() {
        default_files()?
    } else {
        args.files
    };

    let plans = files
        .iter()
        .map(|path| manifest::plan(path, &next, args.if_newer))
        .collect::<Result<Vec<_>, _>>()?;
    for plan in &plans {
        plan.write()?;
        report(plan);
    }

    let changed = plans.iter().any(manifest::Plan::changed);
    let previous = plans
        .first()
        .map(|plan| plan.previous.as_str())
        .unwrap_or_default();
    let written: Vec<String> = plans
        .iter()
        .flat_map(manifest::Plan::written)
        .map(|path| path.display().to_string())
        .collect();
    github_output(&[
        ("version", next.to_string().as_str()),
        ("previous", previous),
        ("changed", if changed { "true" } else { "false" }),
        (
            "prerelease",
            if next.pre.is_empty() { "false" } else { "true" },
        ),
        ("files", written.join("\n").as_str()),
    ])
}

fn report(plan: &manifest::Plan) {
    let path = plan.path.display();
    if plan.changed() {
        println!("{path}: {} -> {}", plan.previous, plan.next);
    }
    if let Some(note) = &plan.note {
        println!("{path}: {note}");
    }
    if let Some(lock) = &plan.lock {
        let noun = if lock.entries == 1 {
            "entry"
        } else {
            "entries"
        };
        println!(
            "{}: {} {noun} -> {}",
            lock.path.display(),
            lock.entries,
            plan.next
        );
    }
}

fn github_output(pairs: &[(&str, &str)]) -> Result<(), String> {
    let Some(path) = env::var_os("GITHUB_OUTPUT").filter(|path| !path.is_empty()) else {
        return Ok(());
    };
    let path = Path::new(&path);
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|err| format!("{}: {err}", path.display()))?;
    // Multi-line values need the heredoc form; the delimiter must not occur in the value.
    let text: String = pairs
        .iter()
        .map(|(key, value)| {
            if value.contains('\n') {
                let delimiter = format!("release_version_{}", std::process::id());
                format!("{key}<<{delimiter}\n{value}\n{delimiter}\n")
            } else {
                format!("{key}={value}\n")
            }
        })
        .collect();
    file.write_all(text.as_bytes())
        .map_err(|err| format!("{}: {err}", path.display()))
}

fn main() -> ExitCode {
    match parse_args(env::args().skip(1)) {
        Ok(Command::Help) => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(Command::Version) => {
            println!("release-version {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Ok(Command::Run(args)) => match run(args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => {
                eprintln!("error: {message}");
                ExitCode::FAILURE
            }
        },
        Err(message) => {
            eprintln!("error: {message}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Command, String> {
        parse_args(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn parses_flags_and_repeated_files() {
        let Ok(Command::Run(args)) = parse(&[
            "v1.2.3",
            "--file",
            "a/package.json",
            "--file=b/Cargo.toml",
            "--strict",
            "--if-newer",
        ]) else {
            panic!("expected a run command");
        };
        assert_eq!(args.tag, "v1.2.3");
        assert_eq!(
            args.files,
            [
                PathBuf::from("a/package.json"),
                PathBuf::from("b/Cargo.toml")
            ]
        );
        assert!(args.strict && args.if_newer);
    }

    #[test]
    fn rejects_bad_usage() {
        assert!(parse(&[]).is_err());
        assert!(parse(&["v1", "v2"]).is_err());
        assert!(parse(&["v1", "--nope"]).is_err());
        assert!(parse(&["v1", "--file"]).is_err());
    }
}
