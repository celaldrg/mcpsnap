//! `mcpsnap replay`: start the server again, send it the recorded client
//! traffic and compare every response with the snapshot.

use crate::cassette::{self, Cassette, Dir, Entry, Kind};
use crate::diff::{self, Change, Pattern};
use crate::json::{self, Value};
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

pub struct Opts {
    pub cmd: Vec<String>,
    pub ignore: Vec<Pattern>,
    pub timeout_ms: u64,
    pub show_stderr: bool,
}

#[derive(Debug)]
pub enum Outcome {
    Match,
    Changed(Vec<Change>),
    /// Recorded a response but got none within the timeout.
    Missing,
    /// Got a response although the recording had none.
    Unexpected,
    /// Neither recording nor replay has a response.
    Unanswered,
}

pub struct Step {
    pub id: String,
    pub label: String,
    pub rec_ms: Option<u64>,
    pub new_ms: Option<u64>,
    pub outcome: Outcome,
}

pub struct Report {
    pub steps: Vec<Step>,
    pub noise: Vec<String>,
    pub unanswered_server_requests: Vec<String>,
    pub transcript: Cassette,
}

impl Report {
    pub fn failed(&self) -> bool {
        !self.noise.is_empty()
            || self
                .steps
                .iter()
                .any(|s| matches!(s.outcome, Outcome::Changed(_) | Outcome::Missing | Outcome::Unexpected))
    }
}

enum Msg {
    Line(String, u64),
    Eof,
}

/// Strips the parts of a response that legitimately differ between runs.
fn comparable(m: &Value) -> Value {
    match m {
        Value::Obj(pairs) => Value::Obj(
            pairs.iter().filter(|(k, _)| k != "id" && k != "jsonrpc").cloned().collect(),
        ),
        other => other.clone(),
    }
}

pub fn run(c: &Cassette, o: &Opts) -> Result<Report, String> {
    let mut child = Command::new(&o.cmd[0])
        .args(&o.cmd[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(if o.show_stderr { Stdio::inherit() } else { Stdio::null() })
        .spawn()
        .map_err(|e| format!("cannot start {}: {}", o.cmd[0], e))?;
    let start = Instant::now();
    let mut stdin = child.stdin.take().expect("stdin piped");
    let stdout = child.stdout.take().expect("stdout piped");
    let (tx, rx) = mpsc::channel::<Msg>();
    thread::spawn(move || {
        let mut br = BufReader::new(stdout);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match br.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let s = String::from_utf8_lossy(&buf).trim_end_matches(['\r', '\n']).to_string();
                    if !s.trim().is_empty() {
                        let _ = tx.send(Msg::Line(s, start.elapsed().as_millis() as u64));
                    }
                }
            }
        }
        let _ = tx.send(Msg::Eof);
    });

    let mut transcript = Cassette { cmd: o.cmd.clone(), recorded: cassette::now_rfc3339(), entries: vec![] };
    let mut noise = Vec::new();
    let mut unanswered_server_requests = Vec::new();
    let mut steps = Vec::new();
    let mut eof = false;

    // recorded response for each client request, keyed by entry index
    let exchanges = c.exchanges();
    let client_msgs: Vec<(usize, &Entry)> =
        c.entries.iter().enumerate().filter(|(_, e)| e.dir == Dir::C2S && e.msg.is_some()).collect();
    let mut sent_early: HashSet<usize> = HashSet::new();

    let send = |stdin: &mut std::process::ChildStdin, transcript: &mut Cassette, m: &Value| -> bool {
        let line = m.to_json();
        transcript.entries.push(Entry {
            t: start.elapsed().as_millis() as u64,
            dir: Dir::C2S,
            msg: Some(m.clone()),
            raw: None,
        });
        stdin.write_all(line.as_bytes()).is_ok() && stdin.write_all(b"\n").is_ok() && stdin.flush().is_ok()
    };

    for (idx, e) in &client_msgs {
        if sent_early.contains(idx) {
            continue;
        }
        let m = e.msg.as_ref().unwrap();
        let sent_at = start.elapsed().as_millis() as u64;
        let alive = !eof && send(&mut stdin, &mut transcript, m);
        let (id, method) = match e.kind() {
            Kind::Request { id, method } => (id, method),
            _ => continue,
        };
        let rec = exchanges.iter().find(|x| x.req == *idx);
        let rec_resp = rec.and_then(|x| x.resp).and_then(|r| c.entries[r].msg.clone());
        let wait = if rec_resp.is_some() { o.timeout_ms } else { o.timeout_ms.min(1000) };
        let deadline = Instant::now() + Duration::from_millis(wait);
        let mut got: Option<(Value, u64)> = None;
        while alive && !eof && got.is_none() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            match rx.recv_timeout(left) {
                Ok(Msg::Line(line, t)) => {
                    let entry = Entry::from_line(t, Dir::S2C, &line);
                    transcript.entries.push(entry.clone());
                    let v = match entry.msg {
                        Some(v) => v,
                        None => {
                            noise.push(line);
                            continue;
                        }
                    };
                    match cassette::classify(&v) {
                        Kind::Response { id: rid, .. } if rid == id => got = Some((v, t)),
                        Kind::Request { id: sid, method: smethod } => {
                            // server -> client request: answer from the recording
                            let answer = client_msgs.iter().find(|(j, ce)| {
                                !sent_early.contains(j)
                                    && matches!(ce.kind(), Kind::Response { id: ref cid, .. } if *cid == sid)
                            });
                            match answer {
                                Some((j, ce)) => {
                                    sent_early.insert(*j);
                                    send(&mut stdin, &mut transcript, ce.msg.as_ref().unwrap());
                                }
                                None => {
                                    unanswered_server_requests.push(format!("{} {}", sid, smethod));
                                    let err = json::parse(&format!(
                                        r#"{{"jsonrpc":"2.0","id":{},"error":{{"code":-32601,"message":"mcpsnap: no recorded reply for {}"}}}}"#,
                                        sid,
                                        smethod.replace('"', "")
                                    ))
                                    .expect("valid json");
                                    send(&mut stdin, &mut transcript, &err);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Msg::Eof) => eof = true,
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => eof = true,
            }
        }
        let outcome = match (&rec_resp, &got) {
            (Some(old), Some((new, _))) => {
                let ch = diff::diff(&comparable(old), &comparable(new), &o.ignore);
                if ch.is_empty() { Outcome::Match } else { Outcome::Changed(ch) }
            }
            (Some(_), None) => Outcome::Missing,
            (None, Some(_)) => Outcome::Unexpected,
            (None, None) => Outcome::Unanswered,
        };
        steps.push(Step {
            id,
            label: if method.is_empty() { "?".into() } else { cassette::label(m) },
            rec_ms: rec.and_then(|x| x.latency),
            new_ms: got.as_ref().map(|(_, t)| t.saturating_sub(sent_at)),
            outcome,
        });
    }

    // let late output arrive, then close stdin and give the server a moment to exit
    while let Ok(Msg::Line(line, t)) = rx.recv_timeout(Duration::from_millis(150)) {
        let entry = Entry::from_line(t, Dir::S2C, &line);
        if entry.msg.is_none() {
            noise.push(line);
        }
        transcript.entries.push(entry);
    }
    drop(stdin);
    let until = Instant::now() + Duration::from_millis(2000);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < until => thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
    }
    Ok(Report { steps, noise, unanswered_server_requests, transcript })
}
