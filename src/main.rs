// fledge-plugin-bench
//
// Run benchmarks, save a baseline, compare against it, flag regressions.
//
// Communicates with fledge via the fledge-v1 plugin protocol over stdio.
// Capabilities required: exec (to run the language's bench tool) + store
// (to persist the baseline between invocations).
//
// Subcommands:
//   run          (default) Run benchmarks; compare against baseline if present
//   save         Run benchmarks and persist the result as the baseline
//   show         Print the saved baseline
//   clear        Delete the saved baseline

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::process::exit;

#[derive(Deserialize)]
struct InitMessage {
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    project: Option<ProjectInfo>,
    #[serde(default)]
    capabilities: Capabilities,
}

#[derive(Deserialize)]
struct ProjectInfo {
    #[serde(default)]
    root: Option<String>,
    #[serde(default)]
    language: Option<String>,
}

#[derive(Default, Deserialize)]
struct Capabilities {
    #[serde(default)]
    exec: bool,
    #[serde(default)]
    store: bool,
}

#[derive(Deserialize)]
struct ExecResult {
    #[serde(default)]
    code: i32,
    #[serde(default)]
    stdout: String,
    #[serde(default, rename = "stderr")]
    _stderr: String,
}

#[derive(Serialize, Deserialize, Clone)]
struct BenchResult {
    name: String,
    ns_per_op: f64,
}

#[derive(Serialize, Deserialize, Clone)]
struct Baseline {
    schema_version: u32,
    language: String,
    command: String,
    results: Vec<BenchResult>,
    saved_at: String,
}

#[derive(Serialize)]
struct Diff {
    name: String,
    ns_per_op: f64,
    previous: Option<f64>,
    delta_pct: Option<f64>,
    regression: bool,
    new: bool,
}

struct Options {
    sub: String,
    json: bool,
    threshold: f64,
    lang: Option<String>,
}

const COMMANDS: &[(&str, &str)] = &[
    ("rust", "cargo bench"),
    ("go", "go test -bench=. -benchmem -run=^$ ./..."),
    ("node", "npm run bench --silent"),
    ("python", "pytest --benchmark-only -q"),
];

fn main() {
    let init = read_init();
    let opts = parse_args(&init.args);

    // `show` and `clear` don't need exec.
    if opts.sub == "show" {
        let b = if init.capabilities.store {
            load_baseline()
        } else {
            None
        };
        match b {
            None => output_line("No baseline saved."),
            Some(b) => {
                if opts.json {
                    output_line(
                        &serde_json::to_string(&json!({
                            "schema_version": 1,
                            "action": "bench_show",
                            "baseline": b,
                        }))
                        .unwrap(),
                    );
                } else {
                    print_baseline(&b);
                }
            }
        }
        return;
    }

    if opts.sub == "clear" {
        send(&json!({"type": "store", "key": "baseline", "value": ""}));
        output_line("Baseline cleared.");
        return;
    }

    if !init.capabilities.exec {
        log_err("exec capability not granted; cannot run benchmarks");
        exit(126);
    }

    let project = init.project;
    let root = project
        .as_ref()
        .and_then(|p| p.root.clone())
        .unwrap_or_else(|| ".".into());
    let project_lang = project.as_ref().and_then(|p| p.language.clone());

    let lang = opts
        .lang
        .clone()
        .or(project_lang)
        .or_else(|| detect_lang(&root));

    let lang = match lang.as_deref() {
        Some(l) if COMMANDS.iter().any(|(n, _)| *n == l) => l.to_string(),
        _ => {
            log_err(&format!(
                "Could not detect a supported language in {} (try --lang)",
                root
            ));
            exit(2);
        }
    };

    let command = COMMANDS.iter().find(|(n, _)| **n == *lang).unwrap().1;

    progress(&format!("Running benchmarks ({})", lang));
    let result = exec(command, 1800);
    progress_done();

    if result.code != 0 {
        log_err(&format!("Bench command exited {}", result.code));
        output_line(&result.stdout);
        exit(1);
    }

    let parsed = parse_results(&result.stdout, &lang);
    if parsed.is_empty() {
        log_warn("Could not parse any benchmark results from output");
    }

    let saved_at = current_iso8601();
    let payload = Baseline {
        schema_version: 1,
        language: lang.clone(),
        command: command.to_string(),
        results: parsed.clone(),
        saved_at,
    };

    if opts.sub == "save" {
        if !init.capabilities.store {
            log_err("store capability not granted; cannot save baseline");
            exit(126);
        }
        let raw = serde_json::to_string(&payload).unwrap();
        send(&json!({"type": "store", "key": "baseline", "value": raw}));
        if opts.json {
            output_line(
                &serde_json::to_string(&json!({
                    "schema_version": 1,
                    "action": "bench_save",
                    "language": &lang,
                    "command": command,
                    "results": parsed,
                    "saved_at": payload.saved_at,
                }))
                .unwrap(),
            );
        } else {
            output_line(&format!("Baseline saved: {} benchmark(s).", parsed.len()));
        }
        return;
    }

    // default sub == "run"
    let baseline = if init.capabilities.store {
        load_baseline()
    } else {
        None
    };
    match baseline {
        None => {
            if opts.json {
                output_line(
                    &serde_json::to_string(&json!({
                        "schema_version": 1,
                        "action": "bench_run",
                        "language": &lang,
                        "command": command,
                        "results": parsed,
                        "saved_at": payload.saved_at,
                        "compared": false,
                    }))
                    .unwrap(),
                );
            } else {
                output_line(&format!(
                    "Benchmarks ({}, {} result(s)) — no baseline yet.",
                    lang,
                    parsed.len()
                ));
                for r in &parsed {
                    output_line(&format!("  {:40} {:>12.1} ns/op", r.name, r.ns_per_op));
                }
                output_line("\nRun `fledge bench save` to lock in this baseline.");
            }
        }
        Some(b) => {
            let diffs = compare(&parsed, &b, opts.threshold);
            let regressions: Vec<&Diff> = diffs.iter().filter(|d| d.regression).collect();

            if opts.json {
                output_line(
                    &serde_json::to_string(&json!({
                        "schema_version": 1,
                        "action": "bench_run",
                        "language": &lang,
                        "command": command,
                        "results": parsed,
                        "saved_at": payload.saved_at,
                        "compared": true,
                        "threshold_pct": opts.threshold,
                        "regression_count": regressions.len(),
                        "diffs": diffs,
                    }))
                    .unwrap(),
                );
            } else {
                output_line(&format!(
                    "Benchmarks ({}, {} result(s)):",
                    lang,
                    parsed.len()
                ));
                for d in &diffs {
                    let (marker, delta) = if d.new {
                        ("+", "  (new)".to_string())
                    } else if d.regression {
                        ("!", format!("  ({:+.1}%)", d.delta_pct.unwrap_or(0.0)))
                    } else if let Some(p) = d.delta_pct {
                        (" ", format!("  ({:+.1}%)", p))
                    } else {
                        (" ", String::new())
                    };
                    output_line(&format!(
                        " {} {:40} {:>12.1} ns/op{}",
                        marker, d.name, d.ns_per_op, delta
                    ));
                }
                if !regressions.is_empty() {
                    output_line(&format!(
                        "\n{} regression(s) over {:.1}% threshold",
                        regressions.len(),
                        opts.threshold
                    ));
                }
            }

            if !regressions.is_empty() {
                exit(1);
            }
        }
    }
}

fn parse_args(args: &[String]) -> Options {
    let mut opts = Options {
        sub: "run".to_string(),
        json: false,
        threshold: 10.0,
        lang: None,
    };
    let mut consumed_sub = false;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--json" => opts.json = true,
            "--threshold" => {
                if let Some(v) = args.get(i + 1).and_then(|s| s.parse().ok()) {
                    opts.threshold = v;
                    i += 1;
                }
            }
            "--lang" => {
                if let Some(v) = args.get(i + 1) {
                    opts.lang = Some(v.clone());
                    i += 1;
                }
            }
            other => {
                if !other.starts_with('-') && !consumed_sub {
                    opts.sub = other.to_string();
                    consumed_sub = true;
                }
            }
        }
        i += 1;
    }
    opts
}

fn detect_lang(root: &str) -> Option<String> {
    let r = Path::new(root);
    let markers: &[(&str, &[&str])] = &[
        ("rust", &["Cargo.toml"]),
        ("go", &["go.mod"]),
        ("node", &["package.json"]),
        ("python", &["pyproject.toml"]),
    ];
    for (lang, files) in markers {
        if files.iter().any(|f| r.join(f).exists()) {
            return Some((*lang).to_string());
        }
    }
    None
}

fn parse_results(output: &str, lang: &str) -> Vec<BenchResult> {
    let mut out: Vec<BenchResult> = Vec::new();
    match lang {
        "rust" => {
            // libtest: `test name ... bench: N,NNN ns/iter`
            let re_iter =
                Regex::new(r"(?m)^test\s+(\S+)\s*\.\.\.\s*bench:\s*([\d,]+)\s*ns/iter").unwrap();
            for c in re_iter.captures_iter(output) {
                let n = c.get(1).unwrap().as_str().to_string();
                let raw = c.get(2).unwrap().as_str().replace(',', "");
                if let Ok(v) = raw.parse::<f64>() {
                    out.push(BenchResult {
                        name: n,
                        ns_per_op: v,
                    });
                }
            }
            // Criterion: `bench/group   time:   [low mid high ns]`
            let re_crit =
                Regex::new(r"(?m)^([\w\-:/]+)\s+time:\s*\[\S+\s+\S+\s+([\d.]+)\s+ns\b").unwrap();
            for c in re_crit.captures_iter(output) {
                let n = c.get(1).unwrap().as_str().to_string();
                if let Ok(v) = c.get(2).unwrap().as_str().parse::<f64>() {
                    if !out.iter().any(|r| r.name == n) {
                        out.push(BenchResult {
                            name: n,
                            ns_per_op: v,
                        });
                    }
                }
            }
        }
        "go" => {
            let re = Regex::new(r"(?m)^(Benchmark\S+)\s+\d+\s+([\d.]+)\s+ns/op").unwrap();
            for c in re.captures_iter(output) {
                let n = c.get(1).unwrap().as_str().to_string();
                if let Ok(v) = c.get(2).unwrap().as_str().parse::<f64>() {
                    out.push(BenchResult {
                        name: n,
                        ns_per_op: v,
                    });
                }
            }
        }
        "python" => {
            let re = Regex::new(r"(?m)^(test_\S+)\s+[\d.,]+\s+([\d.]+)\s*(ns|us|ms|s)\b").unwrap();
            for c in re.captures_iter(output) {
                let n = c.get(1).unwrap().as_str().to_string();
                let raw: f64 = match c.get(2).unwrap().as_str().parse() {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let scale = match c.get(3).unwrap().as_str() {
                    "ns" => 1.0,
                    "us" => 1e3,
                    "ms" => 1e6,
                    "s" => 1e9,
                    _ => 1.0,
                };
                out.push(BenchResult {
                    name: n,
                    ns_per_op: raw * scale,
                });
            }
        }
        "node" => {
            // benchmark.js: `name x N,NNN ops/sec`
            let re = Regex::new(r"(?m)^(.+?)\s+x\s+([\d,.]+)\s+ops/sec").unwrap();
            for c in re.captures_iter(output) {
                let n = c.get(1).unwrap().as_str().trim().to_string();
                let ops: f64 = match c.get(2).unwrap().as_str().replace(',', "").parse() {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if ops > 0.0 {
                    out.push(BenchResult {
                        name: n,
                        ns_per_op: 1e9 / ops,
                    });
                }
            }
        }
        _ => {}
    }
    out
}

fn compare(current: &[BenchResult], baseline: &Baseline, threshold_pct: f64) -> Vec<Diff> {
    use std::collections::HashMap;
    let mut by_name: HashMap<&str, f64> = HashMap::new();
    for b in &baseline.results {
        by_name.insert(b.name.as_str(), b.ns_per_op);
    }
    current
        .iter()
        .map(|r| {
            let prev = by_name.get(r.name.as_str()).copied();
            let delta = prev.and_then(|p| {
                if p > 0.0 {
                    Some((r.ns_per_op - p) / p * 100.0)
                } else {
                    None
                }
            });
            let regression = matches!(delta, Some(d) if d > threshold_pct);
            Diff {
                name: r.name.clone(),
                ns_per_op: r.ns_per_op,
                previous: prev,
                delta_pct: delta,
                regression,
                new: prev.is_none(),
            }
        })
        .collect()
}

fn print_baseline(b: &Baseline) {
    output_line(&format!(
        "Baseline ({}, {} benchmarks, saved at {}):",
        b.language,
        b.results.len(),
        b.saved_at
    ));
    for r in &b.results {
        output_line(&format!("  {:40} {:>12.1} ns/op", r.name, r.ns_per_op));
    }
}

fn current_iso8601() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, mo, d, h, mi, s) = epoch_to_ymd(secs);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mo, d, h, mi, s)
}

// Naive epoch → Y-M-D H:M:S (UTC). Good enough for a filesystem timestamp.
fn epoch_to_ymd(mut s: u64) -> (u64, u64, u64, u64, u64, u64) {
    let (sec, mi) = (s % 60, (s / 60) % 60);
    let h = (s / 3600) % 24;
    s /= 86400;
    let mut y = 1970u64;
    loop {
        let dy = if is_leap(y) { 366 } else { 365 };
        if s >= dy {
            s -= dy;
            y += 1;
        } else {
            break;
        }
    }
    let mdays = if is_leap(y) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut mo = 0;
    for (i, &dm) in mdays.iter().enumerate() {
        if s >= dm {
            s -= dm;
        } else {
            mo = i + 1;
            break;
        }
    }
    if mo == 0 {
        mo = 12;
    }
    (y, mo as u64, s + 1, h, mi, sec)
}

fn is_leap(y: u64) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

// ---- protocol I/O -----------------------------------------------------------

fn send(value: &Value) {
    println!("{}", value);
    io::stdout().flush().ok();
}

fn recv() -> Value {
    let mut line = String::new();
    let stdin = io::stdin();
    stdin.lock().read_line(&mut line).ok();
    if line.trim().is_empty() {
        exit(0);
    }
    serde_json::from_str(&line).unwrap_or(Value::Null)
}

fn read_init() -> InitMessage {
    let v = recv();
    serde_json::from_value(v).unwrap_or_else(|e| {
        log_err(&format!("malformed init: {}", e));
        exit(1);
    })
}

fn exec(command: &str, timeout: u64) -> ExecResult {
    send(&json!({"type": "exec", "id": "1", "command": command, "timeout": timeout}));
    let v = recv();
    let value = v.get("value").cloned().unwrap_or(Value::Null);
    serde_json::from_value(value).unwrap_or(ExecResult {
        code: -1,
        stdout: String::new(),
        _stderr: String::new(),
    })
}

fn load_baseline() -> Option<Baseline> {
    send(&json!({"type": "load", "id": "2", "key": "baseline"}));
    let v = recv();
    let raw = v.get("value")?.as_str()?;
    if raw.is_empty() {
        return None;
    }
    serde_json::from_str(raw).ok()
}

fn output_line(text: &str) {
    let mut t = text.to_string();
    if !t.ends_with('\n') {
        t.push('\n');
    }
    send(&json!({"type": "output", "text": t}));
}

fn progress(message: &str) {
    send(&json!({"type": "progress", "message": message}));
}

fn progress_done() {
    send(&json!({"type": "progress", "done": true}));
}

fn log_err(message: &str) {
    send(&json!({"type": "log", "level": "error", "message": message}));
}

fn log_warn(message: &str) {
    send(&json!({"type": "log", "level": "warn", "message": message}));
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- parse_args tests -------------------------------------------------------

    #[test]
    fn parse_args_defaults() {
        let opts = parse_args(&[]);
        assert_eq!(opts.sub, "run");
        assert!(!opts.json);
        assert_eq!(opts.threshold, 10.0);
        assert!(opts.lang.is_none());
    }

    #[test]
    fn parse_args_subcommand() {
        let args: Vec<String> = vec!["save".into()];
        let opts = parse_args(&args);
        assert_eq!(opts.sub, "save");
    }

    #[test]
    fn parse_args_show() {
        let args: Vec<String> = vec!["show".into()];
        let opts = parse_args(&args);
        assert_eq!(opts.sub, "show");
    }

    #[test]
    fn parse_args_clear() {
        let args: Vec<String> = vec!["clear".into()];
        let opts = parse_args(&args);
        assert_eq!(opts.sub, "clear");
    }

    #[test]
    fn parse_args_json_flag() {
        let args: Vec<String> = vec!["--json".into()];
        let opts = parse_args(&args);
        assert!(opts.json);
        assert_eq!(opts.sub, "run"); // still default
    }

    #[test]
    fn parse_args_threshold() {
        let args: Vec<String> = vec!["--threshold".into(), "5.5".into()];
        let opts = parse_args(&args);
        assert_eq!(opts.threshold, 5.5);
    }

    #[test]
    fn parse_args_lang() {
        let args: Vec<String> = vec!["--lang".into(), "go".into()];
        let opts = parse_args(&args);
        assert_eq!(opts.lang, Some("go".to_string()));
    }

    #[test]
    fn parse_args_combined() {
        let args: Vec<String> = vec![
            "save".into(),
            "--json".into(),
            "--threshold".into(),
            "2.0".into(),
            "--lang".into(),
            "rust".into(),
        ];
        let opts = parse_args(&args);
        assert_eq!(opts.sub, "save");
        assert!(opts.json);
        assert_eq!(opts.threshold, 2.0);
        assert_eq!(opts.lang, Some("rust".to_string()));
    }

    #[test]
    fn parse_args_threshold_missing_value() {
        // --threshold at end with no value: threshold stays at default
        let args: Vec<String> = vec!["--threshold".into()];
        let opts = parse_args(&args);
        assert_eq!(opts.threshold, 10.0);
    }

    // ---- detect_lang tests ------------------------------------------------------

    #[test]
    fn detect_lang_rust() {
        let dir = std::env::temp_dir().join("bench_test_detect_rust");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "").unwrap();
        let result = detect_lang(dir.to_str().unwrap());
        assert_eq!(result, Some("rust".to_string()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn detect_lang_go() {
        let dir = std::env::temp_dir().join("bench_test_detect_go");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("go.mod"), "").unwrap();
        let result = detect_lang(dir.to_str().unwrap());
        assert_eq!(result, Some("go".to_string()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn detect_lang_node() {
        let dir = std::env::temp_dir().join("bench_test_detect_node");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("package.json"), "{}").unwrap();
        let result = detect_lang(dir.to_str().unwrap());
        assert_eq!(result, Some("node".to_string()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn detect_lang_python() {
        let dir = std::env::temp_dir().join("bench_test_detect_python");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("pyproject.toml"), "").unwrap();
        let result = detect_lang(dir.to_str().unwrap());
        assert_eq!(result, Some("python".to_string()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn detect_lang_unknown() {
        let dir = std::env::temp_dir().join("bench_test_detect_none");
        std::fs::create_dir_all(&dir).unwrap();
        let result = detect_lang(dir.to_str().unwrap());
        assert_eq!(result, None);
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- parse_results tests (Rust output) --------------------------------------

    #[test]
    fn parse_results_rust_libtest() {
        let output = "\
test bench_parse_small  ... bench:      1,234 ns/iter (+/- 56)
test bench_parse_large  ... bench:     98,765 ns/iter (+/- 120)
";
        let results = parse_results(output, "rust");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].name, "bench_parse_small");
        assert_eq!(results[0].ns_per_op, 1234.0);
        assert_eq!(results[1].name, "bench_parse_large");
        assert_eq!(results[1].ns_per_op, 98765.0);
    }

    #[test]
    fn parse_results_rust_criterion() {
        let output = "\
my-bench/group          time:   [1.23 ns 4.56 ns 7.89 ns]
";
        let results = parse_results(output, "rust");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "my-bench/group");
        assert_eq!(results[0].ns_per_op, 4.56);
    }

    #[test]
    fn parse_results_rust_empty() {
        let output = "running 0 tests\n\ntest result: ok. 0 passed\n";
        let results = parse_results(output, "rust");
        assert!(results.is_empty());
    }

    // ---- parse_results tests (Go output) ----------------------------------------

    #[test]
    fn parse_results_go() {
        let output = "\
goos: linux
goarch: amd64
BenchmarkFib10-8      5000000     200.5 ns/op    0 B/op    0 allocs/op
BenchmarkFib20-8       300000    3100.0 ns/op    0 B/op    0 allocs/op
PASS
";
        let results = parse_results(output, "go");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].name, "BenchmarkFib10-8");
        assert_eq!(results[0].ns_per_op, 200.5);
        assert_eq!(results[1].name, "BenchmarkFib20-8");
        assert_eq!(results[1].ns_per_op, 3100.0);
    }

    #[test]
    fn parse_results_go_empty() {
        let output = "PASS\nok  \tmodule\t0.003s\n";
        let results = parse_results(output, "go");
        assert!(results.is_empty());
    }

    // ---- parse_results tests (Python output) ------------------------------------

    #[test]
    fn parse_results_python() {
        let output = "\
test_sort_small     1000,000  45.2 us
test_sort_large     100,000   1.3 ms
";
        let results = parse_results(output, "python");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].name, "test_sort_small");
        assert!((results[0].ns_per_op - 45_200.0).abs() < 0.1);
        assert_eq!(results[1].name, "test_sort_large");
        assert!((results[1].ns_per_op - 1_300_000.0).abs() < 0.1);
    }

    #[test]
    fn parse_results_python_nanoseconds() {
        let output = "test_fast     10000,000  500.0 ns\n";
        let results = parse_results(output, "python");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].ns_per_op, 500.0);
    }

    // ---- parse_results tests (Node output) --------------------------------------

    #[test]
    fn parse_results_node() {
        let output = "\
array-sort x 1,234,567 ops/sec ±0.50% (95 runs sampled)
string-concat x 5,000,000 ops/sec ±1.23% (90 runs sampled)
";
        let results = parse_results(output, "node");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].name, "array-sort");
        assert!((results[0].ns_per_op - 1e9 / 1_234_567.0).abs() < 0.01);
        assert_eq!(results[1].name, "string-concat");
        assert!((results[1].ns_per_op - 1e9 / 5_000_000.0).abs() < 0.01);
    }

    #[test]
    fn parse_results_node_empty() {
        let output = "No benchmarks found.\n";
        let results = parse_results(output, "node");
        assert!(results.is_empty());
    }

    // ---- compare / threshold tests ----------------------------------------------

    #[test]
    fn compare_no_regression() {
        let baseline = Baseline {
            schema_version: 1,
            language: "rust".into(),
            command: "cargo bench".into(),
            results: vec![
                BenchResult {
                    name: "bench_a".into(),
                    ns_per_op: 100.0,
                },
                BenchResult {
                    name: "bench_b".into(),
                    ns_per_op: 200.0,
                },
            ],
            saved_at: "2026-01-01T00:00:00Z".into(),
        };
        let current = vec![
            BenchResult {
                name: "bench_a".into(),
                ns_per_op: 105.0,
            }, // +5%
            BenchResult {
                name: "bench_b".into(),
                ns_per_op: 195.0,
            }, // -2.5%
        ];
        let diffs = compare(&current, &baseline, 10.0);
        assert_eq!(diffs.len(), 2);
        assert!(!diffs[0].regression);
        assert!(!diffs[1].regression);
    }

    #[test]
    fn compare_with_regression() {
        let baseline = Baseline {
            schema_version: 1,
            language: "rust".into(),
            command: "cargo bench".into(),
            results: vec![BenchResult {
                name: "bench_a".into(),
                ns_per_op: 100.0,
            }],
            saved_at: "2026-01-01T00:00:00Z".into(),
        };
        let current = vec![
            BenchResult {
                name: "bench_a".into(),
                ns_per_op: 115.0,
            }, // +15%
        ];
        let diffs = compare(&current, &baseline, 10.0);
        assert_eq!(diffs.len(), 1);
        assert!(diffs[0].regression);
        assert!((diffs[0].delta_pct.unwrap() - 15.0).abs() < 0.01);
    }

    #[test]
    fn compare_new_benchmark() {
        let baseline = Baseline {
            schema_version: 1,
            language: "rust".into(),
            command: "cargo bench".into(),
            results: vec![BenchResult {
                name: "bench_a".into(),
                ns_per_op: 100.0,
            }],
            saved_at: "2026-01-01T00:00:00Z".into(),
        };
        let current = vec![
            BenchResult {
                name: "bench_a".into(),
                ns_per_op: 100.0,
            },
            BenchResult {
                name: "bench_new".into(),
                ns_per_op: 50.0,
            },
        ];
        let diffs = compare(&current, &baseline, 10.0);
        assert_eq!(diffs.len(), 2);
        assert!(!diffs[0].new);
        assert!(diffs[1].new);
        assert!(!diffs[1].regression); // new benchmarks are not regressions
    }

    #[test]
    fn compare_exact_threshold_not_regression() {
        let baseline = Baseline {
            schema_version: 1,
            language: "rust".into(),
            command: "cargo bench".into(),
            results: vec![BenchResult {
                name: "bench_a".into(),
                ns_per_op: 100.0,
            }],
            saved_at: "2026-01-01T00:00:00Z".into(),
        };
        // Exactly at threshold (10%) should NOT be a regression (> not >=)
        let current = vec![BenchResult {
            name: "bench_a".into(),
            ns_per_op: 110.0,
        }];
        let diffs = compare(&current, &baseline, 10.0);
        assert!(!diffs[0].regression);
    }

    #[test]
    fn compare_improvement_not_flagged() {
        let baseline = Baseline {
            schema_version: 1,
            language: "rust".into(),
            command: "cargo bench".into(),
            results: vec![BenchResult {
                name: "bench_a".into(),
                ns_per_op: 200.0,
            }],
            saved_at: "2026-01-01T00:00:00Z".into(),
        };
        let current = vec![
            BenchResult {
                name: "bench_a".into(),
                ns_per_op: 100.0,
            }, // -50%
        ];
        let diffs = compare(&current, &baseline, 10.0);
        assert!(!diffs[0].regression);
        assert!(diffs[0].delta_pct.unwrap() < 0.0);
    }

    // ---- epoch_to_ymd / is_leap tests -------------------------------------------

    #[test]
    fn is_leap_year() {
        assert!(is_leap(2000));
        assert!(is_leap(2024));
        assert!(!is_leap(1900));
        assert!(!is_leap(2023));
    }

    #[test]
    fn epoch_to_ymd_unix_epoch() {
        let (y, mo, d, h, mi, s) = epoch_to_ymd(0);
        assert_eq!((y, mo, d, h, mi, s), (1970, 1, 1, 0, 0, 0));
    }

    #[test]
    fn epoch_to_ymd_known_date() {
        // 2024-01-01 00:00:00 UTC = 1704067200
        let (y, mo, d, h, mi, s) = epoch_to_ymd(1704067200);
        assert_eq!((y, mo, d), (2024, 1, 1));
        assert_eq!((h, mi, s), (0, 0, 0));
    }

    #[test]
    fn epoch_to_ymd_mid_year() {
        // 2023-06-15 12:30:45 UTC = 1686832245
        let (y, mo, d, h, mi, s) = epoch_to_ymd(1686832245);
        assert_eq!((y, mo, d), (2023, 6, 15));
        assert_eq!((h, mi, s), (12, 30, 45));
    }

    // ---- Diff serialization test ------------------------------------------------

    #[test]
    fn diff_serializes_correctly() {
        let diff = Diff {
            name: "bench_x".into(),
            ns_per_op: 123.4,
            previous: Some(100.0),
            delta_pct: Some(23.4),
            regression: true,
            new: false,
        };
        let json = serde_json::to_value(&diff).unwrap();
        assert_eq!(json["name"], "bench_x");
        assert_eq!(json["regression"], true);
        assert_eq!(json["new"], false);
    }
}
