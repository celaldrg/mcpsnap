use std::path::PathBuf;
use std::process::{Command, Output};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mcpsnap"))
}

fn python() -> &'static str {
    if cfg!(windows) { "python" } else { "python3" }
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("mcpsnap-test-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name)
}

fn run(args: &[&str]) -> Output {
    Command::new(bin()).args(args).current_dir(root()).output().unwrap()
}

fn text(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned() + &String::from_utf8_lossy(&o.stderr)
}

/// Records a fresh cassette by driving `mcpsnap record` with the demo client.
fn record(name: &str, extra: &[&str]) -> PathBuf {
    let out = tmp(name);
    let mut cmd = Command::new(python());
    cmd.current_dir(root()).args(["examples/demo_client.py", "--"]);
    cmd.arg(bin()).args(["record", "--quiet-stderr", "-o"]).arg(&out);
    cmd.args(["--", python(), "examples/notes_server.py"]).args(extra);
    let o = cmd.output().unwrap();
    assert!(o.status.success(), "{}", text(&o));
    out
}

#[test]
fn record_then_replay_matches() {
    let c = record("same.jsonl", &[]);
    let body = std::fs::read_to_string(&c).unwrap();
    assert!(body.lines().next().unwrap().contains("\"mcpsnap\":1"));
    assert_eq!(body.lines().filter(|l| l.contains("\"dir\":\"c2s\"")).count(), 9);
    assert!(body.contains("\"dir\":\"err\""), "stderr is captured");

    let o = run(&["replay", c.to_str().unwrap(), "--", python(), "examples/notes_server.py"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    assert!(text(&o).contains("8 requests · 8 match"));

    let s = run(&["stats", c.to_str().unwrap()]);
    assert_eq!(s.status.code(), Some(0), "{}", text(&s));
    assert!(text(&s).contains("no protocol issues"));
}

#[test]
fn replay_catches_drift() {
    let c = record("drift.jsonl", &[]);
    let o = run(&["replay", c.to_str().unwrap(), "--", python(), "examples/notes_server.py", "--v2"]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{}", out);
    assert!(out.contains("result.tools[name=search_notes].description"), "{}", out);
    assert!(out.contains("result.tools[name=search_notes].inputSchema.required[1]"));
    assert!(out.contains("- result.tools[name=delete_note]"));
    assert!(out.contains("result.serverInfo.version"));
    assert!(out.contains("non-JSON line from the server"));

    let j = run(&["replay", c.to_str().unwrap(), "--json", "--ignore", "result.serverInfo", "--", python(), "examples/notes_server.py", "--v2"]);
    let js = String::from_utf8_lossy(&j.stdout);
    assert!(js.starts_with("{\"ok\":false"), "{}", js);
    assert!(js.contains("\"match\":5"), "{}", js);
    assert!(!js.contains("serverInfo"), "ignored path must not appear: {}", js);
}

#[test]
fn update_accepts_new_snapshot() {
    let c = record("update.jsonl", &[]);
    let p = c.to_str().unwrap();
    let u = run(&["replay", p, "--update", "--", python(), "examples/notes_server.py", "--v2"]);
    assert_eq!(u.status.code(), Some(0), "{}", text(&u));
    let o = run(&["replay", p, "--json", "--", python(), "examples/notes_server.py", "--v2"]);
    let js = String::from_utf8_lossy(&o.stdout);
    assert!(js.contains("\"changed\":0"), "{}", js);
    // the stray stdout line is still a failure
    assert_eq!(o.status.code(), Some(1));
    let s = run(&["stats", p]);
    assert_eq!(s.status.code(), Some(1));
    assert!(text(&s).contains("non-JSON line on server stdout"));
}

#[test]
fn answers_server_requests_from_the_recording() {
    let o = run(&["replay", "tests/roots.cassette.jsonl", "--timeout", "5000", "--", python(), "tests/roots_server.py"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    assert!(text(&o).contains("2 requests · 2 match"));
}

#[test]
fn show_renders_timeline() {
    let o = run(&["show", "examples/notes.cassette.jsonl"]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(0));
    assert!(out.contains("3 tools: add_note, search_notes, delete_note"), "{}", out);
    assert!(out.contains("-32602 Unknown tool: frobnicate"));
}

#[test]
fn usage_errors_exit_2() {
    assert_eq!(run(&["frob"]).status.code(), Some(2));
    assert_eq!(run(&["show", "does-not-exist.jsonl"]).status.code(), Some(2));
    assert_eq!(run(&["record"]).status.code(), Some(2));
    assert_eq!(run(&["replay", "x.jsonl", "--bogus"]).status.code(), Some(2));
    assert_eq!(run(&["--version"]).status.code(), Some(0));
}
