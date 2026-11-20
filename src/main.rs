//! mcpsnap: record a stdio MCP session once, replay it against the server on
//! every change, and fail when a response drifts from the snapshot.

mod cassette;
mod diff;
mod json;
mod record;
mod replay;
mod ui;

use cassette::{Cassette, Dir, Kind};
use diff::{path_string, Change, Pattern};
use json::{obj, Value};
use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::process::exit;
use ui::*;

const VERSION: &str = env!("CARGO_PKG_VERSION");

const HELP: &str = "mcpsnap - snapshot tests for MCP servers

Record a real stdio session between any MCP client and your server once,
then replay it against the server on every change and fail when a response
drifts: renamed tools, changed schemas, new errors, stray stdout output.

USAGE
  mcpsnap record [-o FILE] [--quiet-stderr] -- <server command...>
  mcpsnap replay FILE [--ignore PATH]... [--timeout MS] [--update] [--json] [-- <server command...>]
  mcpsnap show FILE [--full]
  mcpsnap stats FILE [--json]

COMMANDS
  record   Run the server behind a transparent proxy and write every line in
           both directions to a cassette (default: mcpsnap.cassette.jsonl).
           Put it in front of your server in any client config.
  replay   Start the server again (recorded command unless given after --),
           send the recorded client traffic and diff every response.
  show     Print a cassette as a readable timeline.
  stats    Per-method calls, errors, latency and payload size, plus protocol
           issues (non-JSON on stdout, unanswered requests, reused ids).

REPLAY OPTIONS
  --ignore PATH   Skip a part of every response. Dots separate keys, * matches
                  one segment, ** any depth, arrays of tools/prompts/resources
                  are addressed by name:  result.serverInfo.version
                  **.timestamp   result.tools[name=search].description
  --timeout MS    Per-request timeout (default 10000)
  --update        Rewrite the cassette with the new responses (accept drift)
  --stderr        Pass the server's stderr through
  --json          Machine-readable report

GLOBAL
  --color / --no-color, -h/--help, -V/--version

EXIT CODES
  0 ok   1 drift, missing responses or protocol issues   2 usage or I/O error
";

fn die(msg: &str) -> ! {
    eprintln!("mcpsnap: {}", msg);
    exit(2)
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut color = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    // global flags may appear anywhere before "--"
    let dash = args.iter().position(|a| a == "--").unwrap_or(args.len());
    let mut i = 0;
    let mut end = dash;
    while i < end {
        match args[i].as_str() {
            "--color" => { color = true; args.remove(i); end -= 1; }
            "--no-color" => { color = false; args.remove(i); end -= 1; }
            _ => i += 1,
        }
    }
    set_color(color);
    if args.is_empty() || args[0] == "-h" || args[0] == "--help" || args[0] == "help" {
        print!("{}", HELP);
        exit(if args.is_empty() { 2 } else { 0 });
    }
    if args[0] == "-V" || args[0] == "--version" {
        println!("mcpsnap {}", VERSION);
        return;
    }
    let sub = args.remove(0);
    let (opts, cmd) = match args.iter().position(|a| a == "--") {
        Some(p) => (args[..p].to_vec(), args[p + 1..].to_vec()),
        None => (args.clone(), vec![]),
    };
    let code = match sub.as_str() {
        "record" => cmd_record(&opts, cmd),
        "replay" => cmd_replay(&opts, cmd),
        "show" => cmd_show(&opts),
        "stats" => cmd_stats(&opts),
        other => die(&format!("unknown command '{}' (try --help)", other)),
    };
    exit(code)
}

fn load(path: &str) -> Cassette {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| die(&format!("cannot read {}: {}", path, e)));
    Cassette::parse(&text).unwrap_or_else(|e| die(&format!("{}: {}", path, e)))
}

fn one_file(opts: &[String], flags_with_value: &[&str]) -> String {
    let mut files = vec![];
    let mut i = 0;
    while i < opts.len() {
        if flags_with_value.contains(&opts[i].as_str()) {
            i += 2;
            continue;
        }
        if !opts[i].starts_with('-') {
            files.push(opts[i].clone());
        }
        i += 1;
    }
    match files.len() {
        1 => files.remove(0),
        0 => die("missing cassette file"),
        _ => die("expected exactly one cassette file"),
    }
}

fn check_flags(opts: &[String], allowed: &[&str], with_value: &[&str]) {
    let mut i = 0;
    while i < opts.len() {
        let a = opts[i].as_str();
        if with_value.contains(&a) {
            if i + 1 >= opts.len() {
                die(&format!("{} needs a value", a));
            }
            i += 2;
            continue;
        }
        if a.starts_with('-') && !allowed.contains(&a) {
            die(&format!("unknown option {}", a));
        }
        i += 1;
    }
}

fn values(opts: &[String], flag: &str) -> Vec<String> {
    opts.windows(2).filter(|w| w[0] == flag).map(|w| w[1].clone()).collect()
}

// ---------------------------------------------------------------- record

fn cmd_record(opts: &[String], cmd: Vec<String>) -> i32 {
    check_flags(opts, &["--quiet-stderr"], &["-o", "--out"]);
    if cmd.is_empty() {
        die("record needs the server command after --, e.g. mcpsnap record -- node server.js");
    }
    let out = values(opts, "-o").pop().or_else(|| values(opts, "--out").pop()).unwrap_or_else(|| "mcpsnap.cassette.jsonl".into());
    match record::run(record::Opts { out, cmd, quiet_stderr: opts.iter().any(|a| a == "--quiet-stderr") }) {
        Ok(code) => code,
        Err(e) => die(&e),
    }
}

// ---------------------------------------------------------------- show

fn summary(m: &Value) -> String {
    if let Some(r) = m.get("result") {
        if let Some(info) = r.get("serverInfo") {
            let n = info.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let v = info.get("version").and_then(|v| v.as_str()).unwrap_or("?");
            let pv = r.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or("?");
            return format!("{} {} · protocol {}", n, v, pv);
        }
        for (key, what) in [("tools", "tools"), ("prompts", "prompts"), ("resources", "resources"), ("resourceTemplates", "templates")] {
            if let Some(a) = r.get(key).and_then(|a| a.as_arr()) {
                let names: Vec<&str> = a
                    .iter()
                    .filter_map(|t| t.get("name").or_else(|| t.get("uri")).and_then(|n| n.as_str()))
                    .collect();
                let what = if a.len() == 1 { what.trim_end_matches('s') } else { what };
                return format!("{} {}: {}", a.len(), what, clip(&names.join(", "), 60));
            }
        }
        if let Some(c) = r.get("content").and_then(|a| a.as_arr()) {
            let err = r.get("isError") == Some(&Value::Bool(true));
            let text: Vec<&str> = c.iter().filter_map(|x| x.get("text").and_then(|t| t.as_str())).collect();
            let t = clip(&text.join(" ").replace('\n', " "), 56);
            return if err { red(&format!("isError: {}", json::quote(&t))) } else { json::quote(&t) };
        }
        if let Some(c) = r.get("contents").and_then(|a| a.as_arr()) {
            return format!("{} content item(s)", c.len());
        }
        return String::new();
    }
    if let Some(e) = m.get("error") {
        let code = e.get("code").map(|c| c.to_json()).unwrap_or_default();
        let msg = e.get("message").and_then(|v| v.as_str()).unwrap_or("");
        return format!("{} {}", code, msg);
    }
    String::new()
}

fn cmd_show(opts: &[String]) -> i32 {
    check_flags(opts, &["--full"], &[]);
    let full = opts.iter().any(|a| a == "--full");
    let path = one_file(opts, &[]);
    let c = load(&path);
    let ex = c.exchanges();
    println!(
        "{}  {}  {}",
        bold(&accent("mcpsnap")),
        bold(&path),
        dim(&format!("{} · {} messages · {}", c.cmd.join(" "), c.entries.len(), ms(c.duration())))
    );
    for (i, e) in c.entries.iter().enumerate() {
        let t = dim(&pad(&format!("+{}", ms(e.t)), 8));
        let arrow = match e.dir {
            Dir::C2S => cyan("→"),
            Dir::S2C => magenta("←"),
            Dir::Err => dim("·"),
        };
        let size = e.msg.as_ref().map(|m| m.to_json().len()).unwrap_or(0);
        let line = match (&e.msg, e.kind()) {
            (_, Kind::Noise) if e.dir == Dir::Err => dim(&format!("stderr  {}", clip(e.raw.as_deref().unwrap_or(""), 90))),
            (_, Kind::Noise) => red(&format!("✗ non-JSON on stdout: {}", clip(&json::quote(e.raw.as_deref().unwrap_or("")), 70))),
            (Some(m), Kind::Request { id, .. }) => format!("{} {}", dim(&format!("#{}", id)), bold(&cassette::label(m))),
            (Some(m), Kind::Notification { method }) => {
                let _ = m;
                dim(&method)
            }
            (Some(m), Kind::Response { id, error }) => {
                let x = ex.iter().find(|x| x.resp == Some(i));
                let lat = x.and_then(|x| x.latency).map(|l| format!("{:>6}", ms(l))).unwrap_or_default();
                let what = x
                    .map(|x| c.entries[x.req].msg.as_ref().map(cassette::label).unwrap_or_default())
                    .unwrap_or_default();
                let head = if error {
                    red(&format!("#{} error", id))
                } else if m.get("result").and_then(|r| r.get("isError")) == Some(&Value::Bool(true)) {
                    yellow(&format!("#{} result", id))
                } else {
                    green(&format!("#{} result", id))
                };
                format!("{} {}  {}  {}  {}", pad(&head, 9), dim(&pad(&what, 24)), dim(&pad(&bytes(size), 8)), dim(&lat), summary(m))
            }
            (Some(m), Kind::Invalid) => red(&format!("✗ invalid JSON-RPC: {}", clip(&m.to_json(), 70))),
            (None, _) => String::new(),
        };
        println!("  {} {} {}", t, arrow, line);
        if full {
            if let Some(m) = &e.msg {
                println!("{}", dim(&format!("             {}", clip(&m.to_json(), 400))));
            }
        }
    }
    0
}

// ---------------------------------------------------------------- stats

fn percentile(v: &mut [u64], p: f64) -> u64 {
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    let idx = ((v.len() as f64 - 1.0) * p).round() as usize;
    v[idx]
}

fn cmd_stats(opts: &[String]) -> i32 {
    check_flags(opts, &["--json"], &[]);
    let as_json = opts.iter().any(|a| a == "--json");
    let path = one_file(opts, &[]);
    let c = load(&path);
    struct Row { calls: usize, errors: usize, lat: Vec<u64>, bytes: usize }
    let mut rows: BTreeMap<String, Row> = BTreeMap::new();
    for x in c.exchanges().iter().filter(|x| x.from_client) {
        let r = rows.entry(x.method.clone()).or_insert(Row { calls: 0, errors: 0, lat: vec![], bytes: 0 });
        r.calls += 1;
        if let Some(resp) = x.resp {
            let m = c.entries[resp].msg.as_ref().unwrap();
            let tool_err = m.get("result").and_then(|r| r.get("isError")) == Some(&Value::Bool(true));
            if x.error || tool_err {
                r.errors += 1;
            }
            r.bytes += m.to_json().len();
        }
        if let Some(l) = x.latency {
            r.lat.push(l);
        }
    }
    let issues = cassette::issues(&c);
    let notifications = c.entries.iter().filter(|e| matches!(e.kind(), Kind::Notification { .. })).count();
    let stderr_lines = c.entries.iter().filter(|e| e.dir == Dir::Err).count();
    if as_json {
        let methods = rows
            .iter_mut()
            .map(|(k, r)| {
                let p50 = percentile(&mut r.lat, 0.5);
                let max = r.lat.iter().copied().max().unwrap_or(0);
                obj(vec![
                    ("method", Value::Str(k.clone())),
                    ("calls", Value::Num(r.calls.to_string())),
                    ("errors", Value::Num(r.errors.to_string())),
                    ("p50_ms", Value::Num(p50.to_string())),
                    ("max_ms", Value::Num(max.to_string())),
                    ("response_bytes", Value::Num(r.bytes.to_string())),
                ])
            })
            .collect();
        let out = obj(vec![
            ("cassette", Value::Str(path)),
            ("messages", Value::Num(c.entries.len().to_string())),
            ("duration_ms", Value::Num(c.duration().to_string())),
            ("methods", Value::Arr(methods)),
            ("issues", Value::Arr(issues.iter().map(|i| obj(vec![("severity", Value::Str(i.severity.into())), ("text", Value::Str(i.text.clone()))])).collect())),
        ]);
        println!("{}", out.to_json());
        return if issues.iter().any(|i| i.severity == "error") { 1 } else { 0 };
    }
    println!(
        "{}  {}  {}",
        bold(&accent("mcpsnap stats")),
        bold(&path),
        dim(&format!("{} messages · {} notifications · {} stderr lines · {}", c.entries.len(), notifications, stderr_lines, ms(c.duration())))
    );
    println!();
    println!("  {}", dim(&format!("{}{}{}{}{}{}", pad("method", 24), pad("calls", 7), pad("errors", 8), pad("p50", 9), pad("max", 9), "response size")));
    for (k, r) in rows.iter_mut() {
        let p50 = percentile(&mut r.lat, 0.5);
        let max = r.lat.iter().copied().max().unwrap_or(0);
        let errs = if r.errors > 0 { red(&r.errors.to_string()) } else { dim("0") };
        let tokens = if k == "tools/list" { dim(&format!("  ≈{} tokens of tool definitions", r.bytes / 4)) } else { String::new() };
        println!(
            "  {}{}{}{}{}{}{}",
            pad(&bold(k), 24),
            pad(&r.calls.to_string(), 7),
            pad(&errs, 8),
            pad(&ms(p50), 9),
            pad(&ms(max), 9),
            bytes(r.bytes),
            tokens
        );
    }
    println!();
    if issues.is_empty() {
        println!("  {} no protocol issues", green("✓"));
        return 0;
    }
    for i in &issues {
        let mark = if i.severity == "error" { red("✗") } else { yellow("!") };
        println!("  {} {}", mark, i.text);
    }
    if issues.iter().any(|i| i.severity == "error") { 1 } else { 0 }
}

// ---------------------------------------------------------------- replay

fn short(v: &Value) -> String {
    clip(&v.to_json(), 76)
}

fn change_json(ch: &Change) -> Value {
    match ch {
        Change::Changed(p, a, b) => obj(vec![("op", Value::Str("changed".into())), ("path", Value::Str(path_string(p))), ("old", a.clone()), ("new", b.clone())]),
        Change::Added(p, b) => obj(vec![("op", Value::Str("added".into())), ("path", Value::Str(path_string(p))), ("new", b.clone())]),
        Change::Removed(p, a) => obj(vec![("op", Value::Str("removed".into())), ("path", Value::Str(path_string(p))), ("old", a.clone())]),
    }
}

fn cmd_replay(opts: &[String], cmd: Vec<String>) -> i32 {
    check_flags(opts, &["--update", "--json", "--stderr"], &["--ignore", "--timeout"]);
    let path = one_file(opts, &["--ignore", "--timeout"]);
    let c = load(&path);
    let cmd = if cmd.is_empty() { c.cmd.clone() } else { cmd };
    if cmd.is_empty() {
        die("the cassette has no recorded command; pass one after --");
    }
    let timeout_ms = values(opts, "--timeout").pop().map(|t| t.parse().unwrap_or_else(|_| die("--timeout expects milliseconds"))).unwrap_or(10_000);
    let ignore: Vec<Pattern> = values(opts, "--ignore").iter().map(|s| Pattern::new(s)).collect();
    let o = replay::Opts { cmd: cmd.clone(), ignore, timeout_ms, show_stderr: opts.iter().any(|a| a == "--stderr") };
    let rep = replay::run(&c, &o).unwrap_or_else(|e| die(&e));
    let update = opts.iter().any(|a| a == "--update");

    let (mut ok, mut changed, mut missing, mut other) = (0, 0, 0, 0);
    for s in &rep.steps {
        match s.outcome {
            replay::Outcome::Match => ok += 1,
            replay::Outcome::Changed(_) => changed += 1,
            replay::Outcome::Missing => missing += 1,
            _ => other += 1,
        }
    }
    let _ = other;

    if opts.iter().any(|a| a == "--json") {
        let steps = rep
            .steps
            .iter()
            .map(|s| {
                let (name, changes) = match &s.outcome {
                    replay::Outcome::Match => ("match", vec![]),
                    replay::Outcome::Changed(ch) => ("changed", ch.iter().map(change_json).collect()),
                    replay::Outcome::Missing => ("missing", vec![]),
                    replay::Outcome::Unexpected => ("unexpected", vec![]),
                    replay::Outcome::Unanswered => ("unanswered", vec![]),
                };
                obj(vec![
                    ("id", json::parse(&s.id).unwrap_or(Value::Null)),
                    ("request", Value::Str(s.label.clone())),
                    ("outcome", Value::Str(name.into())),
                    ("recorded_ms", s.rec_ms.map(|v| Value::Num(v.to_string())).unwrap_or(Value::Null)),
                    ("replay_ms", s.new_ms.map(|v| Value::Num(v.to_string())).unwrap_or(Value::Null)),
                    ("changes", Value::Arr(changes)),
                ])
            })
            .collect();
        let out = obj(vec![
            ("ok", Value::Bool(!rep.failed())),
            ("cassette", Value::Str(path.clone())),
            ("command", Value::Arr(cmd.iter().map(|s| Value::Str(s.clone())).collect())),
            ("summary", obj(vec![("requests", Value::Num(rep.steps.len().to_string())), ("match", Value::Num(ok.to_string())), ("changed", Value::Num(changed.to_string())), ("missing", Value::Num(missing.to_string()))])),
            ("steps", Value::Arr(steps)),
            ("stdout_noise", Value::Arr(rep.noise.iter().map(|s| Value::Str(s.clone())).collect())),
        ]);
        println!("{}", out.to_json());
    } else {
        println!("{}  {} {} {}", bold(&accent("mcpsnap replay")), bold(&path), dim("→"), dim(&cmd.join(" ")));
        println!();
        for s in &rep.steps {
            let timing = match (s.rec_ms, s.new_ms) {
                (Some(a), Some(b)) => dim(&format!("{} {} {}", ms(a), "→", ms(b))),
                (None, Some(b)) => dim(&ms(b)),
                _ => String::new(),
            };
            let name = pad(&format!("{} {}", dim(&format!("#{}", s.id)), s.label), 34);
            match &s.outcome {
                replay::Outcome::Match => println!("  {} {} {}", green("✓"), name, timing),
                replay::Outcome::Changed(ch) => {
                    println!("  {} {} {}  {}", red("✗"), bold(&name), red(&format!("{} change{}", ch.len(), if ch.len() == 1 { "" } else { "s" })), timing);
                    for c in ch.iter().take(12) {
                        match c {
                            Change::Changed(p, a, b) => {
                                println!("      {} {}", yellow("~"), path_string(p));
                                println!("          {} {}", red("-"), red(&short(a)));
                                println!("          {} {}", green("+"), green(&short(b)));
                            }
                            Change::Added(p, b) => {
                                let ps = path_string(p);
                                let room = 88usize.saturating_sub(ps.chars().count()).max(16);
                                println!("      {} {}  {}", green("+"), ps, dim(&clip(&b.to_json(), room)))
                            }
                            Change::Removed(p, a) => {
                                let ps = path_string(p);
                                let room = 88usize.saturating_sub(ps.chars().count()).max(16);
                                println!("      {} {}  {}", red("-"), ps, dim(&clip(&a.to_json(), room)))
                            }
                        }
                    }
                    if ch.len() > 12 {
                        println!("      {}", dim(&format!("… {} more", ch.len() - 12)));
                    }
                }
                replay::Outcome::Missing => println!("  {} {} {}", red("✗"), bold(&name), red("no response (timeout or server exited)")),
                replay::Outcome::Unexpected => println!("  {} {} {}", yellow("!"), name, yellow("answered now, unanswered in the recording")),
                replay::Outcome::Unanswered => println!("  {} {} {}", dim("·"), name, dim("unanswered in both runs")),
            }
        }
        for n in &rep.noise {
            println!("  {} {} {}", red("✗"), bold("stdout"), red(&format!("non-JSON line from the server: {}", clip(&json::quote(n), 60))));
        }
        for r in &rep.unanswered_server_requests {
            println!("  {} server request {} has no recorded reply; answered with an error", yellow("!"), r);
        }
        println!();
        let mut parts = vec![format!("{} requests", rep.steps.len()), green(&format!("{} match", ok))];
        parts.push(if changed > 0 { red(&format!("{} changed", changed)) } else { dim("0 changed") });
        parts.push(if missing > 0 { red(&format!("{} missing", missing)) } else { dim("0 missing") });
        if !rep.noise.is_empty() {
            parts.push(red(&format!("{} stdout noise", rep.noise.len())));
        }
        println!("  {}", parts.join(&dim(" · ")));
    }
    if update {
        if let Err(e) = std::fs::write(&path, rep.transcript.to_text()) {
            die(&format!("cannot write {}: {}", path, e));
        }
        if !opts.iter().any(|a| a == "--json") {
            println!("  {} cassette updated with the new responses", accent("↻"));
        }
        return 0;
    }
    if rep.failed() { 1 } else { 0 }
}
