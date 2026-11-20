//! Structural diff between two JSON values with MCP-aware array matching and
//! glob-style ignore paths.

use crate::json::Value;

#[derive(Clone, Debug, PartialEq)]
pub enum Seg {
    Key(String),
    /// Array element: index plus an optional identity such as `name=search`.
    Idx(usize, Option<(String, String)>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    Changed(Vec<Seg>, Value, Value),
    Added(Vec<Seg>, Value),
    Removed(Vec<Seg>, Value),
}

pub fn path_string(p: &[Seg]) -> String {
    let mut s = String::new();
    for seg in p {
        match seg {
            Seg::Key(k) => {
                if !s.is_empty() {
                    s.push('.');
                }
                s.push_str(k);
            }
            Seg::Idx(_, Some((k, v))) => s.push_str(&format!("[{}={}]", k, v)),
            Seg::Idx(i, None) => s.push_str(&format!("[{}]", i)),
        }
    }
    if s.is_empty() {
        s.push('$');
    }
    s
}

/// An ignore pattern such as `result.serverInfo.version`, `**.timestamp`,
/// `result.tools.*.annotations` or `result.tools[name=search].description`.
#[derive(Clone, Debug)]
pub struct Pattern(Vec<String>);

impl Pattern {
    pub fn new(src: &str) -> Pattern {
        let norm = src.replace('[', ".").replace(']', "");
        Pattern(norm.split('.').filter(|s| !s.is_empty()).map(String::from).collect())
    }
    pub fn matches(&self, path: &[Seg]) -> bool {
        glob(&self.0, path)
    }
}

fn seg_matches(pat: &str, seg: &Seg) -> bool {
    if pat == "*" {
        return true;
    }
    match seg {
        Seg::Key(k) => pat == k,
        Seg::Idx(i, ident) => {
            pat == i.to_string()
                || ident.as_ref().map_or(false, |(k, v)| pat == v || pat == format!("{}={}", k, v))
        }
    }
}

fn glob(pat: &[String], path: &[Seg]) -> bool {
    match pat.first() {
        None => true, // a pattern matches everything below it
        Some(p) if p == "**" => (0..=path.len()).any(|i| glob(&pat[1..], &path[i..])),
        Some(p) => !path.is_empty() && seg_matches(p, &path[0]) && glob(&pat[1..], &path[1..]),
    }
}

/// Arrays whose elements are all objects with one of these string fields are
/// matched by that field instead of by position (tools, prompts, resources...).
const IDENTITY_KEYS: &[&str] = &["name", "uri", "uriTemplate", "id"];

fn identity_key(a: &[Value], b: &[Value]) -> Option<&'static str> {
    if a.is_empty() && b.is_empty() {
        return None;
    }
    'next: for key in IDENTITY_KEYS {
        let mut seen = std::collections::HashSet::new();
        for (side, v) in a.iter().map(|v| (0, v)).chain(b.iter().map(|v| (1, v))) {
            match v.get(key).and_then(|x| x.as_str()) {
                Some(s) => {
                    if !seen.insert((side, s.to_string())) {
                        continue 'next; // not unique, can't use it
                    }
                }
                None => continue 'next,
            }
        }
        return Some(key);
    }
    None
}

pub fn diff(old: &Value, new: &Value, ignore: &[Pattern]) -> Vec<Change> {
    let mut out = Vec::new();
    walk(old, new, &mut Vec::new(), ignore, &mut out);
    out
}

fn ignored(path: &[Seg], ignore: &[Pattern]) -> bool {
    ignore.iter().any(|p| p.matches(path))
}

fn walk(old: &Value, new: &Value, path: &mut Vec<Seg>, ignore: &[Pattern], out: &mut Vec<Change>) {
    if ignored(path, ignore) {
        return;
    }
    match (old, new) {
        (Value::Obj(a), Value::Obj(b)) => {
            for (k, va) in a {
                path.push(Seg::Key(k.clone()));
                match new.get(k) {
                    Some(vb) => walk(va, vb, path, ignore, out),
                    None => {
                        if !ignored(path, ignore) {
                            out.push(Change::Removed(path.clone(), va.clone()))
                        }
                    }
                }
                path.pop();
            }
            for (k, vb) in b {
                if old.get(k).is_none() {
                    path.push(Seg::Key(k.clone()));
                    if !ignored(path, ignore) {
                        out.push(Change::Added(path.clone(), vb.clone()));
                    }
                    path.pop();
                }
            }
        }
        (Value::Arr(a), Value::Arr(b)) => match identity_key(a, b) {
            Some(key) => {
                let id = |v: &Value| v.get(key).and_then(|x| x.as_str()).unwrap_or("").to_string();
                for (i, va) in a.iter().enumerate() {
                    let ident = id(va);
                    let found = b.iter().position(|vb| id(vb) == ident);
                    path.push(Seg::Idx(found.unwrap_or(i), Some((key.to_string(), ident))));
                    match found {
                        Some(j) => walk(va, &b[j], path, ignore, out),
                        None => {
                            if !ignored(path, ignore) {
                                out.push(Change::Removed(path.clone(), va.clone()))
                            }
                        }
                    }
                    path.pop();
                }
                for (j, vb) in b.iter().enumerate() {
                    let ident = id(vb);
                    if !a.iter().any(|va| id(va) == ident) {
                        path.push(Seg::Idx(j, Some((key.to_string(), ident))));
                        if !ignored(path, ignore) {
                            out.push(Change::Added(path.clone(), vb.clone()));
                        }
                        path.pop();
                    }
                }
            }
            None => {
                for i in 0..a.len().max(b.len()) {
                    path.push(Seg::Idx(i, None));
                    match (a.get(i), b.get(i)) {
                        (Some(x), Some(y)) => walk(x, y, path, ignore, out),
                        (Some(x), None) => {
                            if !ignored(path, ignore) {
                                out.push(Change::Removed(path.clone(), x.clone()))
                            }
                        }
                        (None, Some(y)) => {
                            if !ignored(path, ignore) {
                                out.push(Change::Added(path.clone(), y.clone()))
                            }
                        }
                        (None, None) => {}
                    }
                    path.pop();
                }
            }
        },
        _ => {
            if !old.same(new) {
                out.push(Change::Changed(path.clone(), old.clone(), new.clone()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::parse;

    fn d(a: &str, b: &str, ign: &[&str]) -> Vec<String> {
        let pats: Vec<Pattern> = ign.iter().map(|s| Pattern::new(s)).collect();
        diff(&parse(a).unwrap(), &parse(b).unwrap(), &pats)
            .iter()
            .map(|c| match c {
                Change::Changed(p, ..) => format!("~{}", path_string(p)),
                Change::Added(p, _) => format!("+{}", path_string(p)),
                Change::Removed(p, _) => format!("-{}", path_string(p)),
            })
            .collect()
    }

    #[test]
    fn tools_matched_by_name_not_position() {
        let a = r#"{"tools":[{"name":"a","d":"x"},{"name":"b","d":"y"}]}"#;
        let b = r#"{"tools":[{"name":"b","d":"y2"},{"name":"c","d":"z"}]}"#;
        assert_eq!(d(a, b, &[]), vec!["-tools[name=a]", "~tools[name=b].d", "+tools[name=c]"]);
    }

    #[test]
    fn positional_arrays() {
        assert_eq!(d("[1,2,3]", "[1,5]", &[]), vec!["~[1]", "-[2]"]);
        assert_eq!(d(r#"{"a":1}"#, r#"{"a":1.0}"#, &[]), Vec::<String>::new());
    }

    #[test]
    fn ignore_patterns() {
        let a = r#"{"result":{"serverInfo":{"version":"1"},"tools":[{"name":"a","ts":1,"d":"x"}]}}"#;
        let b = r#"{"result":{"serverInfo":{"version":"2"},"tools":[{"name":"a","ts":2,"d":"y"}]}}"#;
        assert_eq!(d(a, b, &[]).len(), 3);
        assert_eq!(d(a, b, &["result.serverInfo.version", "**.ts"]), vec!["~result.tools[name=a].d"]);
        assert!(d(a, b, &["result.serverInfo", "result.tools[name=a]"]).is_empty());
        assert!(d(a, b, &["result.serverInfo", "result.tools.*.d", "**.ts"]).is_empty());
        assert!(d(a, b, &["result.serverInfo", "result.tools.0", "**.ts"]).is_empty());
    }
}
