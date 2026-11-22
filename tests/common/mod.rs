use std::path::PathBuf;
use std::process::Command;

pub fn binary() -> PathBuf {
    std::env::var_os("LINT_GATE_BIN").map_or_else(
        || PathBuf::from(env!("CARGO_BIN_EXE_lint-gate")),
        PathBuf::from,
    )
}

pub fn without_python() -> tempfile::TempDir {
    let tools = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        let git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|directory| directory.join("git"))
            .find(|path| path.is_file())
            .expect("Git is required to test the Action");
        std::os::unix::fs::symlink(git.canonicalize().unwrap(), tools.path().join("git")).unwrap();
    }
    tools
}

pub fn command(tools: &tempfile::TempDir) -> Command {
    let mut command = Command::new(binary());
    command.env("PATH", tools.path());
    command
}
