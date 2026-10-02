use std::process::Command;

#[test]
fn via_data_exposes_fetch_build_validate_and_inspect() {
    let help = Command::new(env!("CARGO_BIN_EXE_via-data"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(help.status.success());
    let help = String::from_utf8_lossy(&help.stdout);
    for command in ["fetch", "build", "validate", "inspect"] {
        assert!(help.contains(command));
    }

    let build_help = Command::new(env!("CARGO_BIN_EXE_via-data"))
        .args(["build", "--help"])
        .output()
        .unwrap();
    assert!(build_help.status.success());
    let build_help = String::from_utf8_lossy(&build_help.stdout);
    assert!(build_help.contains("--input"));
    assert!(!build_help.contains("--output"));

    let validate_help = Command::new(env!("CARGO_BIN_EXE_via-data"))
        .args(["validate", "--help"])
        .output()
        .unwrap();
    assert!(validate_help.status.success());
    assert!(String::from_utf8_lossy(&validate_help.stdout).contains("--static"));
}

#[test]
fn via_route_uses_a_single_static_artifact_argument() {
    let help = Command::new(env!("CARGO_BIN_EXE_via-route"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(help.status.success());
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(help.contains("--static"));
    assert!(help.contains("--modes"));
    assert!(!help.contains("--network"));
}
