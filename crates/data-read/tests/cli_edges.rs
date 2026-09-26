//! Remaining CLI edge cases: missing flag values, default fallbacks, and the
//! human-readable sample printing (sub kind + empty title path).

use std::process::Command;

fn bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_data-read"));
    c.env_remove("NO_COLOR");
    c
}

fn out(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

#[test]
fn cli_days_without_value_defaults_to_7() {
    // "--days" as the LAST arg: value missing → unwrap_or(7.0) path
    // --days as the FINAL arg: no value at all → default 7-day window, exit 0
    let o = bin()
        .args([
            "scan",
            "--json",
            "--hermes-home",
            "/tmp/none",
            "--prime-dir",
            "",
            "--days",
        ])
        .output()
        .unwrap();
    assert!(o.status.success());
    assert!(
        out(&o).contains("elapsed_s"),
        "--days without value falls back"
    );
}

#[test]
fn cli_days_non_numeric_defaults_to_7() {
    let o = bin()
        .args([
            "scan",
            "--days",
            "banana",
            "--json",
            "--hermes-home",
            "/tmp/none",
            "--prime-dir",
            "",
        ])
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "bad numeric still runs (default window)"
    );
}

#[test]
fn cli_prime_dir_without_value_disables_prime() {
    let o = bin()
        .args([
            "scan",
            "--json",
            "--hermes-home",
            "/tmp/none",
            "--prime-dir",
        ])
        .output()
        .unwrap();
    assert!(o.status.success());
    let stdout = out(&o);
    assert!(
        stdout.contains("\"prime_files\": 0") || stdout.contains("\"prime_files\":0"),
        "missing value → prime disabled, got: {stdout}"
    );
}

#[test]
fn cli_hermes_home_without_value_uses_default() {
    // last-arg --hermes-home: value None → default Sources path stays
    // (Sources::default() already set it). Must still exit 0.
    let o = bin()
        .args(["scan", "--hermes-home", "--json"])
        .output()
        .unwrap();
    assert!(o.status.success());
}

#[test]
fn cli_human_output_prints_samples_and_kinds() {
    // build a real prime session so the sample block prints with a title and kind
    let base = std::env::temp_dir().join(format!("dataread-cli-samples-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    // timestamps anchored to NOW so a 1-day window catches the file
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let ts = |offset: u64| chrono_like(now - offset);
    fn chrono_like(epoch: u64) -> String {
        // minimal epoch → ISO for the prime loader (UTC)
        let days = epoch / 86400;
        let secs = epoch % 86400;
        // civil from days (Hinnant)
        let z = days as i64 + 719468;
        let era = if z >= 0 { z } else { z - 146096 } / 146097;
        let doe = z - era * 146097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = if m <= 2 { y + 1 } else { y };
        format!(
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
            secs / 3600,
            (secs % 3600) / 60,
            secs % 60
        )
    }
    std::fs::write(
        base.join("sess-sample.jsonl"),
        format!(
            "{{\"type\":\"session\",\"timestamp\":\"{t0}\"}}\n\
             {{\"type\":\"message\",\"timestamp\":\"{t0}\",\"message\":{{\"role\":\"user\",\"content\":\"sample question\"}}}}\n\
             {{\"type\":\"message\",\"timestamp\":\"{t1}\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"a reply\"}}]}}}}\n",
            t0 = ts(60),
            t1 = ts(30),
        ),
    )
    .unwrap();
    let o = bin()
        .args([
            "scan",
            "--days",
            "1",
            "--hermes-home",
            "/tmp/none",
            "--prime-dir",
            base.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(o.status.success());
    let stdout = out(&o);
    assert!(stdout.contains("scanned 1 sessions"), "got: {stdout}");
    assert!(stdout.contains("human: 1"), "got: {stdout}");
    assert!(
        stdout.contains("sample question"),
        "sample block prints the title, got: {stdout}"
    );
}

#[test]
fn cli_scan_with_no_args_uses_all_defaults() {
    // bare "scan": hermes home + prime dir from Sources::default() — on this
    // machine those exist; contract = exits 0 and prints something
    let o = bin().arg("scan").output().unwrap();
    assert!(o.status.success());
    assert!(!out(&o).is_empty());
}
