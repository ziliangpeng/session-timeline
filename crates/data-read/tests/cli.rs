//! CLI tests: run the real `data-read` binary against synthetic fixtures.
//! Locks Q10 (CLI never exits non-zero on skipped/broken sources) and the
//! flag surface of `scan`.

use std::process::{Command, Output};

fn bin() -> Command {
    // target dir picked up via CARGO_MANIFEST_DIR + profile from env
    let mut c = Command::new(env!("CARGO_BIN_EXE_data-read"));
    c.env_remove("NO_COLOR");
    c
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn tmp_sources(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let base = std::env::temp_dir().join(format!("dataread-cli-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let prime = base.join("prime");
    let home = base.join("hermes");
    std::fs::create_dir_all(prime.clone()).unwrap();
    std::fs::create_dir_all(home.join("profiles")).unwrap();
    (home, prime)
}

#[test]
fn cli_scan_help_exits_zero() {
    let o = bin().arg("--help").output().unwrap();
    assert!(o.status.success());
    assert!(out(&o).contains("scan") || !out(&o).is_empty());
}

#[test]
fn cli_scan_json_prints_counts() {
    let (_home, prime) = tmp_sources("json");
    // epoch-anchored fixtures won't match --days windows (anchored at now), so
    // we exercise the JSON path with a nonexistent hermes home + empty prime:
    // counts are 0 but the contract (json, exit 0) is what we lock here.
    let o = bin()
        .args([
            "scan",
            "--days",
            "1",
            "--hermes-home",
            "/tmp/definitely-not-here",
            "--prime-dir",
            prime.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "exit 0 even with no sources");
    let stdout = out(&o);
    assert!(
        stdout.contains("\"sessions\": 0") || stdout.contains("\"sessions\":0"),
        "got: {stdout}"
    );
    assert!(stdout.contains("elapsed_s"));
}

#[test]
fn cli_scan_human_readable_output() {
    let (home, prime) = tmp_sources("human");
    let o = bin()
        .args([
            "scan",
            "--days",
            "7",
            "--hermes-home",
            home.to_str().unwrap(),
            "--prime-dir",
            prime.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(o.status.success());
    let stdout = out(&o);
    assert!(stdout.contains("scanned"), "got: {stdout}");
    assert!(stdout.contains("sessions"));
}

#[test]
fn cli_broken_hermes_db_is_warning_not_failure() {
    let (home, prime) = tmp_sources("broken");
    let bad = home.join("profiles/bad/state.db");
    std::fs::create_dir_all(bad.parent().unwrap()).unwrap();
    std::fs::write(&bad, b"garbage not sqlite").unwrap();
    let o = bin()
        .args([
            "scan",
            "--days",
            "7",
            "--hermes-home",
            home.to_str().unwrap(),
            "--prime-dir",
            prime.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "Q10: broken source → exit 0");
}

#[test]
fn cli_unknown_flag_exits_nonzero() {
    let o = bin().arg("scan").arg("--bogus").output().unwrap();
    assert!(!o.status.success());
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(stderr.contains("unknown arg"), "got: {stderr}");
}

#[test]
fn cli_prime_dir_empty_string_disables_prime() {
    let (_home, _prime) = tmp_sources("noprime");
    let o = bin()
        .args([
            "scan",
            "--days",
            "1",
            "--hermes-home",
            "/tmp/definitely-not-here",
            "--prime-dir",
            "",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(o.status.success());
    let stdout = out(&o);
    assert!(
        stdout.contains("\"prime_files\": 0") || stdout.contains("\"prime_files\":0"),
        "got: {stdout}"
    );
}

#[test]
fn cli_days_value_parses() {
    // --days 0.5 exercises the f64 parse path
    let (_home, prime) = tmp_sources("halfday");
    let o = bin()
        .args([
            "scan",
            "--days",
            "0.5",
            "--hermes-home",
            "/tmp/definitely-not-here",
            "--prime-dir",
            prime.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(o.status.success());
    assert!(out(&o).contains("elapsed_s"));
}
