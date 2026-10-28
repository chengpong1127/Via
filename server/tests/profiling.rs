use std::{fs, process::Command};

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_via-route"));
    command.args([
        "--static",
        "/nonexistent/via-static.json",
        "--from-lat",
        "25.0",
        "--from-lon",
        "121.5",
        "--to-lat",
        "25.1",
        "--to-lon",
        "121.6",
    ]);
    command
}

#[test]
fn cli_flushes_a_valid_trace_even_when_initialization_later_fails() {
    let path = std::env::temp_dir().join(format!("via-profile-{}.trace.json", std::process::id()));
    let output = command().env("VIA_CHROME_TRACE", &path).output().unwrap();
    assert!(!output.status.success());
    let trace: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert!(trace.is_array() || trace.get("traceEvents").is_some());
    fs::remove_file(path).unwrap();
}

#[test]
fn cli_reports_unwritable_trace_path() {
    let output = command()
        .env("VIA_CHROME_TRACE", "/nonexistent/via/trace.json")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("cannot create VIA_CHROME_TRACE output")
    );
}
