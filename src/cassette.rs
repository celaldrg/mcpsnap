//! The cassette: a JSONL transcript of one stdio MCP session.
//!
//! line 1   {"mcpsnap":1,"cmd":["python3","server.py"],"recorded":"2026-11-20T10:02:11Z"}
//! line 2.. {"t":12,"dir":"c2s","msg":{...}}     client -> server
//!          {"t":40,"dir":"s2c","msg":{...}}     server -> client
//!          {"t":41,"dir":"s2c","raw":"hello"}   non-JSON line on server stdout
//!          {"t":41,"dir":"err","line":"..."}    server stderr

use crate::json::{self, obj, Value};
use std::collections::HashMap;

pub const FORMAT: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Dir {
    C2S,
    S2C,
    Err,
}

impl Dir {
    pub fn tag(self) -> &'static str {
        match self {
            Dir::C2S => "c2s",
            Dir::S2C => "s2c",
            Dir::Err => "err",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub t: u64,
    pub dir: Dir,
    pub msg: Option<Value>,
    /// Raw text for stderr lines and for stdout lines that are not JSON.
    pub raw: Option<String>,
}

impl Entry {
    pub fn from_line(t: u64, dir: Dir, line: &str) -> Entry {
        if dir == Dir::Err {
            return Entry { t, dir, msg: None, raw: Some(line.to_string()) };
        }
        match json::parse(line) {
            Ok(v) => Entry { t, dir, msg: Some(v), raw: None },
            Err(_) => Entry { t, dir, msg: None, raw: Some(line.to_string()) },
        }
    }
    pub fn to_line(&self) -> String {
        let mut pairs = vec![
            ("t", Value::Num(self.t.to_string())),
            ("dir", Value::Str(self.dir.tag().into())),
        ];
        if let Some(m) = &self.msg {
            pairs.push(("msg", m.clone()));
        } else if let Some(r) = &self.raw {
            pairs.push((if self.dir == Dir::Err { "line" } else { "raw" }, Value::Str(r.clone())));
        }
        obj(pairs).to_json()
    }
    pub fn kind(&self) -> Kind {
        match &self.msg {
            None => Kind::Noise,
            Some(m) => classify(m),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Request { id: String, method: String },
    Notification { method: String },
    Response { id: String, error: bool },
    Invalid,
    Noise,
}

pub fn id_key(v: &Value) -> String {
    v.to_json()
}

pub fn classify(m: &Value) -> Kind {
    let method = m.get("method").and_then(|v| v.as_str());
    let id = m.get("id").filter(|v| !matches!(v, Value::Null));
    match (method, id) {
        (Some(meth), Some(id)) => Kind::Request { id: id_key(id), method: meth.to_string() },
        (Some(meth), None) => Kind::Notification { method: meth.to_string() },
        (None, Some(id)) if m.get("result").is_some() || m.get("error").is_some() => {
            Kind::Response { id: id_key(id), error: m.get("error").is_some() }
        }
        _ => Kind::Invalid,
    }
}

pub struct Cassette {
    pub cmd: Vec<String>,
    pub recorded: String,
    pub entries: Vec<Entry>,
}

impl Cassette {
    pub fn header_line(cmd: &[String], recorded: &str) -> String {
        obj(vec![
            ("mcpsnap", Value::Num(FORMAT.to_string())),
            ("cmd", Value::Arr(cmd.iter().map(|s| Value::Str(s.clone())).collect())),
            ("recorded", Value::Str(recorded.to_string())),
        ])
        .to_json()
    }

    pub fn parse(text: &str) -> Result<Cassette, String> {
        let mut c = Cassette { cmd: vec![], recorded: String::new(), entries: vec![] };
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let v = json::parse(line).map_err(|e| format!("line {}: {}", n + 1, e))?;
            if v.get("mcpsnap").is_some() {
                if let Some(a) = v.get("cmd").and_then(|a| a.as_arr()) {
                    c.cmd = a.iter().filter_map(|s| s.as_str().map(String::from)).collect();
                }
                c.recorded = v.get("recorded").and_then(|s| s.as_str()).unwrap_or("").to_string();
                continue;
            }
            let dir = match v.get("dir").and_then(|d| d.as_str()) {
                Some("c2s") => Dir::C2S,
                Some("s2c") => Dir::S2C,
                Some("err") => Dir::Err,
                _ => return Err(format!("line {}: missing or unknown \"dir\"", n + 1)),
            };
            let t = v.get("t").and_then(|t| t.as_f64()).unwrap_or(0.0) as u64;
            let msg = v.get("msg").cloned();
            let raw = v
                .get("raw")
                .or_else(|| v.get("line"))
                .and_then(|s| s.as_str())
                .map(String::from);
            if msg.is_none() && raw.is_none() {
                return Err(format!("line {}: entry has neither \"msg\" nor \"raw\"", n + 1));
            }
            c.entries.push(Entry { t, dir, msg, raw });
        }
        Ok(c)
    }

    pub fn to_text(&self) -> String {
        let mut s = Cassette::header_line(&self.cmd, &self.recorded);
        s.push('\n');
        for e in &self.entries {
            s.push_str(&e.to_line());
            s.push('\n');
        }
        s
    }

    pub fn duration(&self) -> u64 {
        self.entries.last().map(|e| e.t).unwrap_or(0)
    }

    /// Pairs each request with its response, in either direction.
    pub fn exchanges(&self) -> Vec<Exchange> {
        let mut out: Vec<Exchange> = Vec::new();
        let mut open: HashMap<(bool, String), Vec<usize>> = HashMap::new();
        for (i, e) in self.entries.iter().enumerate() {
            match e.kind() {
                Kind::Request { id, method } => {
                    let from_client = e.dir == Dir::C2S;
                    let duplicate = open.get(&(from_client, id.clone())).map_or(false, |v| !v.is_empty());
                    out.push(Exchange {
                        req: i,
                        resp: None,
                        id: id.clone(),
                        method,
                        from_client,
                        latency: None,
                        error: false,
                        duplicate_id: duplicate,
                    });
                    open.entry((from_client, id)).or_default().push(out.len() - 1);
                }
                Kind::Response { id, error } => {
                    // a response travelling s2c answers a client request
                    let from_client = e.dir == Dir::S2C;
                    if let Some(q) = open.get_mut(&(from_client, id)) {
                        if !q.is_empty() {
                            let x = q.remove(0);
                            out[x].resp = Some(i);
                            out[x].error = error;
                            out[x].latency = Some(e.t.saturating_sub(self.entries[out[x].req].t));
                        }
                    }
                }
                _ => {}
            }
        }
        out
    }
}

#[derive(Clone, Debug)]
pub struct Exchange {
    pub req: usize,
    pub resp: Option<usize>,
    pub id: String,
    pub method: String,
    pub from_client: bool,
    pub latency: Option<u64>,
    pub error: bool,
    pub duplicate_id: bool,
}

/// Short human label for a request: "tools/call add_note", "resources/read file:///x".
pub fn label(msg: &Value) -> String {
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("?");
    let params = msg.get("params");
    let extra = match method {
        "tools/call" | "prompts/get" => params.and_then(|p| p.get("name")).and_then(|n| n.as_str()),
        "resources/read" | "resources/subscribe" | "resources/unsubscribe" => {
            params.and_then(|p| p.get("uri")).and_then(|n| n.as_str())
        }
        _ => None,
    };
    match extra {
        Some(x) => format!("{} {}", method, x),
        None => method.to_string(),
    }
}

/// Problems that make a session fragile, independent of any diff.
#[derive(Clone, Debug, PartialEq)]
pub struct Issue {
    pub severity: &'static str, // "error" | "warn"
    pub text: String,
}

pub fn issues(c: &Cassette) -> Vec<Issue> {
    let mut out = Vec::new();
    for e in &c.entries {
        if e.dir == Dir::S2C && e.msg.is_none() {
            out.push(Issue {
                severity: "error",
                text: format!(
                    "non-JSON line on server stdout at +{}ms (strict clients drop the connection): {}",
                    e.t,
                    crate::ui::clip(&json::quote(e.raw.as_deref().unwrap_or("")), 70)
                ),
            });
        }
        if e.dir == Dir::C2S && e.msg.is_none() {
            out.push(Issue { severity: "error", text: format!("non-JSON line from client at +{}ms", e.t) });
        }
        if let Some(m) = &e.msg {
            if m.get("jsonrpc").and_then(|v| v.as_str()) != Some("2.0") {
                out.push(Issue {
                    severity: "error",
                    text: format!("{} message at +{}ms is missing \"jsonrpc\": \"2.0\"", e.dir.tag(), e.t),
                });
            } else if e.kind() == Kind::Invalid {
                out.push(Issue {
                    severity: "error",
                    text: format!("{} message at +{}ms is not a valid JSON-RPC request, notification or response", e.dir.tag(), e.t),
                });
            }
        }
    }
    let ex = c.exchanges();
    let mut answered = std::collections::HashSet::new();
    for x in &ex {
        if let Some(r) = x.resp {
            answered.insert(r);
        }
        let who = if x.from_client { "client" } else { "server" };
        if x.resp.is_none() {
            let cancelled = c.entries.iter().any(|e| {
                e.msg.as_ref().map_or(false, |m| {
                    m.get("method").and_then(|v| v.as_str()) == Some("notifications/cancelled")
                        && m.get("params").and_then(|p| p.get("requestId")).map(id_key).as_deref() == Some(x.id.as_str())
                })
            });
            if !cancelled {
                out.push(Issue {
                    severity: "error",
                    text: format!("{} request {} {} was never answered", who, x.id, x.method),
                });
            }
        }
        if x.duplicate_id {
            out.push(Issue {
                severity: "error",
                text: format!("{} reused id {} while a request with that id was still open", who, x.id),
            });
        }
    }
    for (i, e) in c.entries.iter().enumerate() {
        if let Kind::Response { id, .. } = e.kind() {
            if !answered.contains(&i) {
                out.push(Issue {
                    severity: "warn",
                    text: format!("{} response at +{}ms has id {} that matches no open request", e.dir.tag(), e.t, id),
                });
            }
        }
    }
    out
}

/// Returns an RFC 3339 UTC timestamp for "now" without pulling in a date crate.
pub fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    rfc3339(secs)
}

pub fn rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // civil-from-days (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, m, d, rem / 3600, rem / 60 % 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"mcpsnap":1,"cmd":["srv"],"recorded":"2026-11-20T10:00:00Z"}
{"t":0,"dir":"c2s","msg":{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}}
{"t":30,"dir":"s2c","raw":"booting..."}
{"t":40,"dir":"s2c","msg":{"jsonrpc":"2.0","id":1,"result":{}}}
{"t":41,"dir":"c2s","msg":{"jsonrpc":"2.0","method":"notifications/initialized"}}
{"t":42,"dir":"c2s","msg":{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"x"}}}
{"t":43,"dir":"err","line":"warn: slow"}
"#;

    #[test]
    fn parses_and_roundtrips() {
        let c = Cassette::parse(SAMPLE).unwrap();
        assert_eq!(c.cmd, vec!["srv"]);
        assert_eq!(c.entries.len(), 6);
        let again = Cassette::parse(&c.to_text()).unwrap();
        assert_eq!(again.entries.len(), 6);
        assert_eq!(again.entries[1].raw.as_deref(), Some("booting..."));
    }

    #[test]
    fn pairs_and_finds_issues() {
        let c = Cassette::parse(SAMPLE).unwrap();
        let ex = c.exchanges();
        assert_eq!(ex.len(), 2);
        assert_eq!(ex[0].latency, Some(40));
        assert!(ex[1].resp.is_none());
        let is = issues(&c);
        assert!(is.iter().any(|i| i.text.contains("non-JSON line on server stdout")));
        assert!(is.iter().any(|i| i.text.contains("2 tools/call was never answered")));
        assert_eq!(label(c.entries[4].msg.as_ref().unwrap()), "tools/call x");
    }

    #[test]
    fn dates() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_795_175_331), "2026-11-20T11:48:51Z");
    }
}
