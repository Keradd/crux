use assert_cmd::Command;
use predicates::prelude::*;

fn crux() -> Command {
    Command::cargo_bin("crux").unwrap()
}

fn with_tmp_home() -> (tempfile::TempDir, Command) {
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = crux();
    cmd.env("CRUX_HOME", dir.path()).current_dir(dir.path());
    (dir, cmd)
}

#[test]
fn shows_help() {
    crux()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("CRUX").or(predicate::str::contains("crux")));
}

#[test]
fn shows_version() {
    crux()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("0.4"));
}

#[test]
fn init_creates_project() {
    let (dir, _cmd) = with_tmp_home();
    let mut cmd = crux();
    cmd.env("CRUX_HOME", dir.path())
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success();
    assert!(dir.path().join(".crux").join("config.toml").exists());
}

#[test]
fn doctor_succeeds() {
    let (dir, _cmd) = with_tmp_home();
    let mut cmd = crux();
    cmd.env("CRUX_HOME", dir.path())
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success();

    let mut cmd = crux();
    cmd.env("CRUX_HOME", dir.path())
        .current_dir(dir.path())
        .arg("doctor")
        .assert()
        .success();
}

#[test]
fn search_after_index_finds_code() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let project = dir.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();

    // Init project
    crux()
        .env("CRUX_HOME", &home)
        .current_dir(&project)
        .arg("init")
        .assert()
        .success();

    // Write code file
    let src = project.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("lib.rs"),
        "pub fn greet(name: &str) -> String { format!(\"Hello {name}\") }",
    )
    .unwrap();

    // Index
    crux()
        .env("CRUX_HOME", &home)
        .current_dir(&project)
        .arg("index")
        .assert()
        .success();

    // Search
    crux()
        .env("CRUX_HOME", &home)
        .current_dir(&project)
        .arg("search")
        .arg("--query")
        .arg("greet function")
        .assert()
        .success()
        .stdout(predicate::str::contains("greet"));
}
