//! Building an engine from source (`alkahest engines install --from-source`): git, cmake and
//! ninja run as subprocesses, their output goes to a log file, and only the build products are
//! kept. Never runs unless explicitly requested: it executes the project's build scripts.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio_util::sync::CancellationToken;

use crate::error::{RecipeError, Result};
use crate::recipe::{CargoBuild, SourceBuild, VenvBuild};

/// The programs a source build runs; overridable so tests (and unusual systems) can point at
/// other executables.
#[derive(Debug, Clone)]
pub struct BuildTools {
    /// The `git` program.
    pub git: PathBuf,
    /// The `cmake` program.
    pub cmake: PathBuf,
    /// The `cargo` program, for `cargo` recipe builds.
    pub cargo: PathBuf,
    /// The `uv` program, preferred for Python environments.
    pub uv: PathBuf,
    /// The Python interpreter used when `uv` is absent.
    pub python: PathBuf,
}

impl Default for BuildTools {
    fn default() -> Self {
        Self {
            git: "git".into(),
            cmake: "cmake".into(),
            cargo: "cargo".into(),
            uv: "uv".into(),
            python: "python3".into(),
        }
    }
}

impl BuildTools {
    /// The program for a tool name of a recipe's `source.tools` list.
    fn program(&self, name: &str) -> PathBuf {
        match name {
            "git" => self.git.clone(),
            "cmake" => self.cmake.clone(),
            "cargo" => self.cargo.clone(),
            other => PathBuf::from(other),
        }
    }
}

/// Whether `program` is an executable file, or a name found on `PATH`.
fn tool_exists(program: &Path) -> bool {
    if program.components().count() > 1 {
        return program.is_file();
    }
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let cand = dir.join(program);
        cand.is_file() || cand.with_extension("exe").is_file()
    })
}

/// Checks that every tool the recipe lists is available.
pub(crate) fn check_tools(src: &SourceBuild, tools: &BuildTools) -> Result<()> {
    check_tool_names(&src.tools, tools)
}

/// Checks that every program named in `names` is available.
fn check_tool_names(names: &[String], tools: &BuildTools) -> Result<()> {
    for name in names {
        if !tool_exists(&tools.program(name)) {
            return Err(RecipeError::BuildToolMissing(name.clone()));
        }
    }
    Ok(())
}

/// Clones, configures and builds `src` inside `work`, then leaves the contents of the build's
/// `bin_dir` directly in `work` and removes the checkout. Output of every step is appended to
/// `log`.
pub(crate) async fn build_from_source(
    src: &SourceBuild,
    tools: &BuildTools,
    work: &Path,
    log: &Path,
    cancel: &CancellationToken,
) -> Result<()> {
    check_tools(src, tools)?;
    if let Some(parent) = log.parent() {
        fs::create_dir_all(parent).map_err(|e| RecipeError::io(parent, e))?;
    }
    let checkout = work.join("src");
    fs::create_dir_all(&checkout).map_err(|e| RecipeError::io(&checkout, e))?;
    let cmake = tools.cmake.clone();
    let step = |name: &'static str, program: &Path, args: Vec<String>| Step {
        name,
        program: program.to_path_buf(),
        args,
        cwd: checkout.clone(),
        env: Vec::new(),
    };
    git_checkout(
        &src.git,
        &src.rev,
        src.submodules,
        tools,
        &checkout,
        log,
        cancel,
    )
    .await?;

    let mut configure: Vec<String> = vec![
        "-S".into(),
        ".".into(),
        "-B".into(),
        "build".into(),
        "-DCMAKE_BUILD_TYPE=Release".into(),
    ];
    if src.tools.iter().any(|t| t == "ninja") {
        configure.push("-G".into());
        configure.push("Ninja".into());
    }
    configure.extend(src.cmake_flags.iter().cloned());
    run(&step("cmake configure", &cmake, configure), log, cancel).await?;

    let mut build: Vec<String> = vec![
        "--build".into(),
        "build".into(),
        "--config".into(),
        "Release".into(),
        "--parallel".into(),
    ];
    for t in &src.targets {
        build.push("--target".into());
        build.push(t.clone());
    }
    run(&step("cmake build", &cmake, build), log, cancel).await?;

    // Keep only the products: move them next to where the marker goes, drop the sources.
    let bin = checkout.join(&src.bin_dir);
    let entries = fs::read_dir(&bin).map_err(|e| RecipeError::io(&bin, e))?;
    for entry in entries.flatten() {
        let to = work.join(entry.file_name());
        fs::rename(entry.path(), &to).map_err(|e| RecipeError::io(&to, e))?;
    }
    fs::remove_dir_all(&checkout).map_err(|e| RecipeError::io(&checkout, e))?;
    Ok(())
}

/// Fetches `rev` of `git_url` (shallow) into the empty directory `checkout` and checks it out,
/// with its submodules when asked. Shared by the CMake and Cargo builds.
async fn git_checkout(
    git_url: &str,
    rev: &str,
    submodules: bool,
    tools: &BuildTools,
    checkout: &Path,
    log: &Path,
    cancel: &CancellationToken,
) -> Result<()> {
    let git = &tools.git;
    let step = |name: &'static str, args: Vec<String>| Step {
        name,
        program: git.clone(),
        args,
        cwd: checkout.to_path_buf(),
        env: Vec::new(),
    };
    let strs = |a: &[&str]| a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    run(&step("git init", strs(&["init", "-q"])), log, cancel).await?;
    run(
        &step("git remote", strs(&["remote", "add", "origin", git_url])),
        log,
        cancel,
    )
    .await?;
    run(
        &step("git fetch", strs(&["fetch", "--depth", "1", "origin", rev])),
        log,
        cancel,
    )
    .await?;
    run(
        &step("git checkout", strs(&["checkout", "-q", "FETCH_HEAD"])),
        log,
        cancel,
    )
    .await?;
    if submodules {
        run(
            &step(
                "git submodules",
                strs(&[
                    "submodule",
                    "update",
                    "--init",
                    "--recursive",
                    "--depth",
                    "1",
                ]),
            ),
            log,
            cancel,
        )
        .await?;
    }
    Ok(())
}

/// The `cargo build` arguments of `c`: release profile, locked dependencies, the package and
/// its features.
pub(crate) fn cargo_build_args(c: &CargoBuild) -> Vec<String> {
    let mut args: Vec<String> = ["build", "--release", "--locked", "-p"]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    args.push(c.package.clone());
    if !c.features.is_empty() {
        args.push("--features".into());
        args.push(c.features.join(","));
    }
    args
}

/// File name of the binary `cargo` produces for `bin` on this platform.
fn cargo_bin_file(bin: &str) -> String {
    if cfg!(windows) {
        format!("{bin}.exe")
    } else {
        bin.to_string()
    }
}

/// Fetches `c` into `work/src`, runs `cargo build --release --locked -p <package>` with the
/// target directory inside the checkout, copies the produced binary to the root of `work` and
/// removes the checkout. Output of every step is appended to `log`.
pub(crate) async fn build_cargo(
    c: &CargoBuild,
    tools: &BuildTools,
    work: &Path,
    log: &Path,
    cancel: &CancellationToken,
) -> Result<()> {
    check_tool_names(&c.tools, tools)?;
    if let Some(parent) = log.parent() {
        fs::create_dir_all(parent).map_err(|e| RecipeError::io(parent, e))?;
    }
    let checkout = work.join("src");
    fs::create_dir_all(&checkout).map_err(|e| RecipeError::io(&checkout, e))?;
    git_checkout(&c.git, &c.rev, false, tools, &checkout, log, cancel).await?;

    let target = checkout.join("target");
    let build = Step {
        name: "cargo build",
        program: tools.cargo.clone(),
        args: cargo_build_args(c),
        cwd: checkout.clone(),
        env: vec![("CARGO_TARGET_DIR".into(), target.display().to_string())],
    };
    run(&build, log, cancel).await?;

    let file = cargo_bin_file(c.bin_name());
    let from = target.join("release").join(&file);
    let to = work.join(&file);
    fs::rename(&from, &to).map_err(|e| RecipeError::io(&from, e))?;
    fs::remove_dir_all(&checkout).map_err(|e| RecipeError::io(&checkout, e))?;
    Ok(())
}

/// Creates the virtual environment of `venv` at `dir` (which must not exist yet) and installs
/// its packages. Output of every step is appended to `log`. The caller removes `dir` on error.
pub(crate) async fn build_venv(
    venv: &VenvBuild,
    tools: &BuildTools,
    dir: &Path,
    log: &Path,
    cancel: &CancellationToken,
) -> Result<()> {
    if let Some(parent) = log.parent() {
        fs::create_dir_all(parent).map_err(|e| RecipeError::io(parent, e))?;
    }
    let cwd = dir
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    fs::create_dir_all(&cwd).map_err(|e| RecipeError::io(&cwd, e))?;
    let dir_arg = dir.display().to_string();
    let mut index_args: Vec<String> = Vec::new();
    if let Some(u) = &venv.index_url {
        index_args.extend(["--index-url".to_string(), u.clone()]);
    }
    for u in &venv.extra_index_urls {
        index_args.extend(["--extra-index-url".to_string(), u.clone()]);
    }
    let step = |name: &'static str, program: PathBuf, args: Vec<String>| Step {
        name,
        program,
        args,
        cwd: cwd.clone(),
        env: Vec::new(),
    };

    if tool_exists(&tools.uv) {
        let mut create = vec!["venv".to_string()];
        if let Some(py) = &venv.python {
            create.extend(["--python".to_string(), py.clone()]);
        }
        create.push(dir_arg.clone());
        run(&step("uv venv", tools.uv.clone(), create), log, cancel).await?;
        let mut install = vec![
            "pip".to_string(),
            "install".to_string(),
            "--python".to_string(),
            dir.join(VENV_BIN).join("python").display().to_string(),
        ];
        install.extend(index_args);
        install.extend(venv.packages.iter().cloned());
        run(
            &step("uv pip install", tools.uv.clone(), install),
            log,
            cancel,
        )
        .await
    } else if tool_exists(&tools.python) {
        run(
            &step(
                "python -m venv",
                tools.python.clone(),
                vec!["-m".into(), "venv".into(), dir_arg],
            ),
            log,
            cancel,
        )
        .await?;
        let mut install = vec!["install".to_string()];
        install.extend(index_args);
        install.extend(venv.packages.iter().cloned());
        run(
            &step("pip install", dir.join(VENV_BIN).join("pip"), install),
            log,
            cancel,
        )
        .await
    } else {
        Err(RecipeError::BuildToolMissing("uv (or python3)".into()))
    }
}

/// Directory of a virtual environment that holds its executables.
#[cfg(windows)]
const VENV_BIN: &str = "Scripts";
/// Directory of a virtual environment that holds its executables.
#[cfg(not(windows))]
const VENV_BIN: &str = "bin";

/// One subprocess of the build.
struct Step {
    name: &'static str,
    program: PathBuf,
    args: Vec<String>,
    cwd: PathBuf,
    /// Extra environment variables for the process.
    env: Vec<(String, String)>,
}

/// Runs `step`, appending its output to `log`. Cancelling kills the process.
async fn run(step: &Step, log: &Path, cancel: &CancellationToken) -> Result<()> {
    let open = || -> Result<File> {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
            .map_err(|e| RecipeError::io(log, e))
    };
    let mut header = open()?;
    writeln!(
        header,
        "$ ({}) {} {}",
        step.cwd.display(),
        step.program.display(),
        step.args.join(" ")
    )
    .map_err(|e| RecipeError::io(log, e))?;
    let out = open()?;
    let err = out.try_clone().map_err(|e| RecipeError::io(log, e))?;
    let mut child = tokio::process::Command::new(&step.program)
        .args(&step.args)
        .current_dir(&step.cwd)
        .envs(step.env.iter().map(|(k, v)| (k, v)))
        // Build scripts are third-party code; the server's API key stays out of their reach.
        .env_remove("ALKAHEST_API_KEY")
        .env_remove("ALKAHEST_API_KEY_FILE")
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err))
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| RecipeError::io(&step.program, e))?;
    let status = tokio::select! {
        s = child.wait() => s.map_err(|e| RecipeError::io(&step.program, e))?,
        () = cancel.cancelled() => {
            let _ = child.kill().await;
            return Err(RecipeError::BuildFailed {
                step: step.name.to_string(),
                status: "cancelled".into(),
                log: log.to_path_buf(),
                tail: String::new(),
            });
        }
    };
    if status.success() {
        return Ok(());
    }
    let text = fs::read_to_string(log).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    let tail = lines[lines.len().saturating_sub(15)..].join("\n");
    Err(RecipeError::BuildFailed {
        step: step.name.to_string(),
        status: status.to_string(),
        log: log.to_path_buf(),
        tail,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn cargo_build(features: &[&str]) -> CargoBuild {
        CargoBuild {
            git: "https://example.org/x.git".into(),
            rev: "v1".into(),
            package: "srv".into(),
            features: features.iter().map(|f| (*f).to_string()).collect(),
            bin: None,
            tools: vec!["git".into(), "cargo".into()],
        }
    }

    #[test]
    fn cargo_args_are_release_locked_and_scoped_to_the_package() {
        assert_eq!(
            cargo_build_args(&cargo_build(&[])),
            ["build", "--release", "--locked", "-p", "srv"]
        );
        assert_eq!(
            cargo_build_args(&cargo_build(&["cuda", "kai"])),
            [
                "build",
                "--release",
                "--locked",
                "-p",
                "srv",
                "--features",
                "cuda,kai"
            ]
        );
    }

    #[test]
    fn binary_name_defaults_to_the_package() {
        let mut c = cargo_build(&[]);
        assert_eq!(c.bin_name(), "srv");
        c.bin = Some("other".into());
        assert_eq!(c.bin_name(), "other");
    }

    #[tokio::test]
    async fn missing_cargo_is_reported_before_anything_runs() {
        let tools = BuildTools {
            cargo: "/nonexistent/definitely-not-cargo".into(),
            ..BuildTools::default()
        };
        let dir = std::env::temp_dir().join(format!("alk-cargo-missing-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let log = dir.join("build.log");
        let err = build_cargo(
            &cargo_build(&[]),
            &tools,
            &dir,
            &log,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(matches!(&err, RecipeError::BuildToolMissing(t) if t == "cargo"));
        assert!(!dir.join("src").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
