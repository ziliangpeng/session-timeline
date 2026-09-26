//! CLI smoke for the data-read component: scan sources, print counts and a few
//! sample sessions. Usage:
//!   data-read scan [--days N] [--hermes-home DIR] [--prime-dir DIR] [--json]

use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut days: f64 = 7.0;
    let mut sources = data_read::Sources::default();
    let mut json = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "scan" => {}
            "--days" => {
                i += 1;
                days = args.get(i).and_then(|v| v.parse().ok()).unwrap_or(7.0);
            }
            "--hermes-home" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    sources.hermes_home = PathBuf::from(v);
                }
            }
            "--prime-dir" => {
                i += 1;
                match args.get(i) {
                    Some(v) if v.is_empty() => sources.prime_dir = None,
                    Some(v) => sources.prime_dir = Some(PathBuf::from(v)),
                    None => sources.prime_dir = None,
                }
            }
            "--json" => json = true,
            other => {
                eprintln!("unknown arg: {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }

    let t1 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let t0 = t1 - days * 86400.0;

    let report = data_read::scan_reported(&sources, t0, t1);
    if json {
        println!(
            "{}",
            serde_json::json!({
                "sessions": report.sessions.len(),
                "hermes_profiles": report.hermes_profiles,
                "prime_files": report.prime_files,
                "elapsed_s": report.elapsed_s,
            })
        );
        return;
    }
    println!(
        "scanned {} sessions from {} hermes profiles + {} prime files in {:.2}s",
        report.sessions.len(),
        report.hermes_profiles,
        report.prime_files,
        report.elapsed_s
    );
    let mut by_kind = (0usize, 0usize);
    for s in &report.sessions {
        match s.kind {
            data_read::model::SessionKind::Human => by_kind.0 += 1,
            data_read::model::SessionKind::Sub => by_kind.1 += 1,
        }
    }
    println!(
        "  human: {}  sub: {}  spans: {}",
        by_kind.0,
        by_kind.1,
        report.sessions.iter().map(|s| s.spans.len()).sum::<usize>()
    );
    let mut samples: Vec<&data_read::model::Session> = report.sessions.iter().collect();
    samples.sort_by(|a, b| b.t_start.partial_cmp(&a.t_start).unwrap());
    println!("\nsample (latest 5):");
    for s in samples.iter().take(5) {
        let title = s.title.clone().unwrap_or_default();
        let title: String = title.chars().take(48).collect();
        println!(
            "  {:<18} {:<6} {:>5} spans  {title}",
            s.id,
            format!("{:?}", s.kind).to_lowercase(),
            s.spans.len()
        );
    }
}
