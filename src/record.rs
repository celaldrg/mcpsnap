//! `mcpsnap record`: a transparent stdio proxy that writes every line in both
//! directions to a cassette while the client and server talk normally.

use crate::cassette::{Cassette, Dir, Entry};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

pub struct Opts {
    pub out: String,
    pub cmd: Vec<String>,
    pub quiet_stderr: bool,
}

fn read_lines<R: Read>(r: R, mut each: impl FnMut(&[u8]) -> bool) {
    let mut br = BufReader::new(r);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match br.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                if !each(&buf) {
                    break;
                }
            }
        }
    }
}

fn strip_eol(b: &[u8]) -> String {
    let mut s = String::from_utf8_lossy(b).into_owned();
    while s.ends_with('\n') || s.ends_with('\r') {
        s.pop();
    }
    s
}

pub fn run(o: Opts) -> Result<i32, String> {
    let mut file = File::create(&o.out).map_err(|e| format!("cannot create {}: {}", o.out, e))?;
    writeln!(file, "{}", Cassette::header_line(&o.cmd, &crate::cassette::now_rfc3339()))
        .map_err(|e| e.to_string())?;
    let log = Arc::new(Mutex::new(file));
    let start = Instant::now();

    let mut child = Command::new(&o.cmd[0])
        .args(&o.cmd[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start {}: {}", o.cmd[0], e))?;

    let write_entry = {
        let log = log.clone();
        move |dir: Dir, line: &str| {
            let e = Entry::from_line(start.elapsed().as_millis() as u64, dir, line);
            if let Ok(mut f) = log.lock() {
                let _ = writeln!(f, "{}", e.to_line());
                let _ = f.flush();
            }
        }
    };

    // client -> server
    let mut child_in = child.stdin.take().expect("stdin piped");
    let w1 = write_entry.clone();
    thread::spawn(move || {
        read_lines(std::io::stdin(), |b| {
            let line = strip_eol(b);
            if !line.trim().is_empty() {
                w1(Dir::C2S, &line);
            }
            child_in.write_all(b).is_ok() && child_in.flush().is_ok()
        });
        drop(child_in); // EOF to the server
    });

    // server -> client
    let child_out = child.stdout.take().expect("stdout piped");
    let w2 = write_entry.clone();
    let t_out = thread::spawn(move || {
        let mut stdout = std::io::stdout();
        read_lines(child_out, |b| {
            let ok = stdout.write_all(b).is_ok() && stdout.flush().is_ok();
            let line = strip_eol(b);
            if !line.trim().is_empty() {
                w2(Dir::S2C, &line);
            }
            ok
        });
    });

    // server stderr: keep passing it through, and keep a copy
    let child_err = child.stderr.take().expect("stderr piped");
    let w3 = write_entry;
    let quiet = o.quiet_stderr;
    let t_err = thread::spawn(move || {
        let mut stderr = std::io::stderr();
        read_lines(child_err, |b| {
            if !quiet {
                let _ = stderr.write_all(b);
            }
            w3(Dir::Err, &strip_eol(b));
            true
        });
    });

    let status = child.wait().map_err(|e| e.to_string())?;
    let _ = t_out.join();
    let _ = t_err.join();
    Ok(status.code().unwrap_or(1))
}
