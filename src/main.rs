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
        let b = if init.capabilities.store { load_baseline() } else { None };
        match b {
            None => output_line("No baseline saved."),
            Some(b) => {
                if opts.json {
                    output_line(&serde_json::to_string(&json!({
                        "schema_version": 1,
                        "action": "bench_show",
                        "baseline": b,
                    })).unwrap());
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
    let root = project.as_ref().and_then(|p| p.root.clone()).unwrap_or_else(|| ".".into());
    let project_lang = project.as_ref().and_then(|p| p.language.clone());

    let lang = opts.lang.clone()
        .or(project_lang)
        .or_else(|| detect_lang(&root));

    let lang = match lang.as_deref() {
        Some(l) if COMMANDS.iter().any(|(n, _)| *n == l) => l.to_string(),
        _ => {
            log_err(&format!("Could not detect a supported language in {} (try --lang)", root));
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
            output_line(&serde_json::to_string(&json!({
                "schema_version": 1,
                "action": "bench_save",
                "language": &lang,
                "command": command,
                "results": parsed,
                "saved_at": payload.saved_at,
            })).unwrap());
        } else {
            output_line(&format!("Baseline saved: {} benchmark(s).", parsed.len()));
        }
        return;
    }

    // default sub == "run"
    let baseline = if init.capabilities.store { load_baseline() } else { None };
    match baseline {
        None => {
            if opts.json {
                output_line(&serde_json::to_string(&json!({
                    "schema_version": 1,
                    "action": "bench_run",
                    "language": &lang,
                    "command": command,
                    "results": parsed,
                    "saved_at": payload.saved_at,
                    "compared": false,
                })).unwrap());
            } else {
                output_line(&format!(
                    "Benchmarks ({}, {} result(s)) — no baseline yet.",
                    lang, parsed.len()
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
                output_line(&serde_json::to_string(&json!({
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
                })).unwrap());
            } else {
                output_line(&format!("Benchmarks ({}, {} result(s)):", lang, parsed.len()));
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
                        regressions.len(), opts.threshold
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
            let re_iter = Regex::new(r"(?m)^test\s+(\S+)\s*\.\.\.\s*bench:\s*([\d,]+)\s*ns/iter").unwrap();
            for c in re_iter.captures_iter(output) {
                let n = c.get(1).unwrap().as_str().to_string();
                let raw = c.get(2).unwrap().as_str().replace(',', "");
                if let Ok(v) = raw.parse::<f64>() {
                    out.push(BenchResult { name: n, ns_per_op: v });
                }
            }
            // Criterion: `bench/group   time:   [low mid high ns]`
            let re_crit = Regex::new(r"(?m)^([\w\-:/]+)\s+time:\s*\[\S+\s+\S+\s+([\d.]+)\s+ns\b").unwrap();
            for c in re_crit.captures_iter(output) {
                let n = c.get(1).unwrap().as_str().to_string();
                if let Ok(v) = c.get(2).unwrap().as_str().parse::<f64>() {
                    if !out.iter().any(|r| r.name == n) {
                        out.push(BenchResult { name: n, ns_per_op: v });
                    }
                }
            }
        }
        "go" => {
            let re = Regex::new(r"(?m)^(Benchmark\S+)\s+\d+\s+([\d.]+)\s+ns/op").unwrap();
            for c in re.captures_iter(output) {
                let n = c.get(1).unwrap().as_str().to_string();
                if let Ok(v) = c.get(2).unwrap().as_str().parse::<f64>() {
                    out.push(BenchResult { name: n, ns_per_op: v });
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
                out.push(BenchResult { name: n, ns_per_op: raw * scale });
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
                    out.push(BenchResult { name: n, ns_per_op: 1e9 / ops });
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
            let delta = prev.and_then(|p| if p > 0.0 { Some((r.ns_per_op - p) / p * 100.0) } else { None });
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
        b.language, b.results.len(), b.saved_at
    ));
    for r in &b.results {
        output_line(&format!("  {:40} {:>12.1} ns/op", r.name, r.ns_per_op));
    }
}

fn current_iso8601() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
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
        if s >= dy { s -= dy; y += 1; } else { break; }
    }
    let mdays = if is_leap(y) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut mo = 0;
    for (i, &dm) in mdays.iter().enumerate() {
        if s >= dm { s -= dm; } else { mo = i + 1; break; }
    }
    if mo == 0 { mo = 12; }
    (y, mo as u64, s + 1, h, mi, sec)
}

fn is_leap(y: u64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
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
