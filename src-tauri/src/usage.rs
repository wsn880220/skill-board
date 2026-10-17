// 从现有会话日志里统计 skill 使用情况
// 计数单位是“会话 × skill”；解析结果累计保存在 cache.usage，原始日志被删也不丢
use crate::scanner::{home, Skill};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

static SKILL_PATH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"((?:~|/)[^\s"'\\`]*?/)?([^\s"'\\`/]+)/SKILL\.md"#).unwrap());
static COMMAND_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<command-name>/?([^<\s]+)</command-name>").unwrap());
static SLASH_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^/([A-Za-z0-9_:.-]+)").unwrap());
static DOLLAR_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:^|\s)\$([A-Za-z][A-Za-z0-9_:.-]*)").unwrap());
static CWD_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""cwd":"((?:[^"\\]|\\.)*)""#).unwrap());

const CODEX_CALL_TYPES: &[&str] = &["function_call", "custom_tool_call", "local_shell_call"];

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Record {
    pub tool: String,
    pub session: String,
    pub skill: String,
    pub ts: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct FileStamp {
    pub size: u64,
    pub mtime: i64,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct Summary {
    pub total: u32,
    #[serde(rename = "byTool")]
    pub by_tool: HashMap<String, u32>,
    #[serde(rename = "lastUsed")]
    pub last_used: i64,
}

fn h(p: &str) -> PathBuf {
    home().join(p)
}

fn list_jsonl(dir: &Path) -> Vec<PathBuf> {
    fn walk(d: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = fs::read_dir(d) else { return };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let p = e.path();
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                walk(&p, out);
            } else {
                let n = e.file_name().to_string_lossy().to_string();
                if n.ends_with(".jsonl") || n.ends_with(".jsonl.zst") {
                    out.push(p);
                }
            }
        }
    }
    let mut out = vec![];
    walk(dir, &mut out);
    out
}

// 逐行读取；旧的 Codex 会话会被压缩成 .jsonl.zst
fn each_line(file: &Path, mut f: impl FnMut(&str)) -> std::io::Result<()> {
    let raw = File::open(file)?;
    let reader: Box<dyn Read> = if file.to_string_lossy().ends_with(".zst") {
        Box::new(zstd::stream::read::Decoder::new(raw)?)
    } else {
        Box::new(raw)
    };
    let mut br = BufReader::with_capacity(1 << 20, reader);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        if br.read_until(b'\n', &mut buf)? == 0 {
            break;
        }
        let line = String::from_utf8_lossy(&buf);
        f(line.trim_end_matches(['\n', '\r']));
    }
    Ok(())
}

// 类似 Node 的 path.join：去掉 . 和 ..
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

// 把日志里出现的 skill 引用（路径或名字）归到扫描出来的某个 skill 上
struct Resolver {
    by_file: HashMap<PathBuf, String>,
    by_dir: HashMap<String, String>,
    by_name: HashSet<String>,
    real_cache: std::cell::RefCell<HashMap<PathBuf, Option<PathBuf>>>,
}

impl Resolver {
    fn new(skills: &[Skill]) -> Self {
        let mut by_file = HashMap::new();
        let mut by_dir = HashMap::new();
        let mut by_name = HashSet::new();
        for s in skills {
            for f in s.files.iter().chain(s.paths.iter()) {
                if let Some(d) = Path::new(f).parent() {
                    by_file.insert(d.to_path_buf(), s.id.clone());
                }
            }
            for d in &s.dir_names {
                by_dir.entry(d.clone()).or_insert_with(|| s.id.clone());
            }
            by_name.insert(s.id.clone());
        }
        Resolver { by_file, by_dir, by_name, real_cache: Default::default() }
    }

    fn from_path(&self, prefix: Option<&str>, dir_name: &str) -> Option<String> {
        if let Some(prefix) = prefix {
            let expanded = match prefix.strip_prefix('~') {
                Some(rest) => format!("{}{}", home().to_string_lossy(), rest),
                None => prefix.to_string(),
            };
            let full = normalize(&Path::new(&expanded).join(dir_name));
            if let Some(id) = self.by_file.get(&full) {
                return Some(id.clone());
            }
            let real = self
                .real_cache
                .borrow_mut()
                .entry(full.clone())
                .or_insert_with(|| fs::canonicalize(&full).ok())
                .clone();
            if let Some(id) = real.and_then(|r| self.by_file.get(&r).cloned()) {
                return Some(id);
            }
        }
        let d = dir_name.to_lowercase();
        self.by_dir.get(&d).cloned().or_else(|| self.by_name.get(&d).cloned())
    }

    fn from_name(&self, name: &str) -> Option<String> {
        let lower = name.to_lowercase();
        let n = lower.rsplit(':').next().unwrap_or(&lower);
        self.by_name.get(n).cloned().or_else(|| self.by_dir.get(n).cloned())
    }

    fn from_text(&self, text: &str) -> HashSet<String> {
        SKILL_PATH_RE
            .captures_iter(text)
            .filter_map(|m| self.from_path(m.get(1).map(|x| x.as_str()), &m[2]))
            .collect()
    }
}

fn record(usage: &mut HashMap<String, Record>, tool: &str, session: &str, skill: &str, ts: i64) {
    let key = format!("{tool}|{session}|{skill}");
    match usage.get_mut(&key) {
        Some(prev) => {
            if ts > prev.ts {
                prev.ts = ts;
            }
        }
        None => {
            usage.insert(key, Record { tool: tool.into(), session: session.into(), skill: skill.into(), ts });
        }
    }
}

fn to_ms(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Number(n)) => {
            let x = n.as_f64().unwrap_or(0.0);
            if x < 1e12 { (x * 1000.0) as i64 } else { x as i64 }
        }
        Some(Value::String(s)) => chrono::DateTime::parse_from_rfc3339(s).map(|d| d.timestamp_millis()).unwrap_or(0),
        _ => 0,
    }
}

fn stamp(file: &Path) -> Option<FileStamp> {
    let m = fs::metadata(file).ok()?;
    let mtime = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as i64;
    Some(FileStamp { size: m.len(), mtime })
}

fn file_stem(file: &Path) -> String {
    let n = file.file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
    n.trim_end_matches(".zst").trim_end_matches(".jsonl").to_string()
}

fn parse_codex_session(file: &Path, usage: &mut HashMap<String, Record>, r: &Resolver) -> std::io::Result<()> {
    let mut session = file_stem(file);
    each_line(file, |line| {
        if line.contains("\"session_meta\"") {
            if let Ok(o) = serde_json::from_str::<Value>(line) {
                if let Some(s) = o.pointer("/payload/session_id").or_else(|| o.pointer("/payload/id")).and_then(|v| v.as_str()) {
                    session = s.to_string();
                }
            }
            return;
        }
        if !line.contains("SKILL.md") {
            return;
        }
        let Ok(o) = serde_json::from_str::<Value>(line) else { return };
        if o.get("type").and_then(|v| v.as_str()) != Some("response_item") {
            return;
        }
        let Some(payload) = o.get("payload") else { return };
        let pt = payload.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if !CODEX_CALL_TYPES.contains(&pt) {
            return;
        }
        let ts = to_ms(o.get("timestamp"));
        for id in r.from_text(&payload.to_string()) {
            record(usage, "Codex", &session, &id, ts);
        }
    })
}

fn parse_claude_session(file: &Path, usage: &mut HashMap<String, Record>, r: &Resolver) -> std::io::Result<()> {
    let fallback = file_stem(file);
    each_line(file, |line| {
        let has_cmd = line.contains("<command-name>");
        if !line.contains("\"Skill\"") && !line.contains("SKILL.md") && !has_cmd {
            return;
        }
        let Ok(o) = serde_json::from_str::<Value>(line) else { return };
        let session = o.get("sessionId").and_then(|v| v.as_str()).unwrap_or(&fallback).to_string();
        let ts = to_ms(o.get("timestamp"));
        let typ = o.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let content = o.pointer("/message/content");
        if typ == "assistant" {
            let Some(arr) = content.and_then(|c| c.as_array()) else { return };
            for c in arr {
                if c.get("type").and_then(|v| v.as_str()) != Some("tool_use") {
                    continue;
                }
                let input = c.get("input");
                if c.get("name").and_then(|v| v.as_str()) == Some("Skill") {
                    if let Some(id) = input.and_then(|i| i.get("skill")).and_then(|v| v.as_str()).and_then(|s| r.from_name(s)) {
                        record(usage, "Claude", &session, &id, ts);
                    }
                } else if let Some(input) = input {
                    for id in r.from_text(&input.to_string()) {
                        record(usage, "Claude", &session, &id, ts);
                    }
                }
            }
        } else if typ == "user" && has_cmd {
            let text = match content {
                Some(Value::String(s)) => s.clone(),
                Some(v) => v.to_string(),
                None => String::new(),
            };
            for m in COMMAND_RE.captures_iter(&text) {
                if let Some(id) = r.from_name(&m[1]) {
                    record(usage, "Claude", &session, &id, ts);
                }
            }
        }
    })
}

// Claude 的 history.jsonl：用户手动敲的 /命令
fn parse_claude_history(file: &Path, usage: &mut HashMap<String, Record>, r: &Resolver) -> std::io::Result<()> {
    each_line(file, |line| {
        let Ok(o) = serde_json::from_str::<Value>(line) else { return };
        let display = o.get("display").and_then(|v| v.as_str()).unwrap_or("");
        let Some(session) = o.get("sessionId").and_then(|v| v.as_str()) else { return };
        if let Some(m) = SLASH_RE.captures(display) {
            if let Some(id) = r.from_name(&m[1]) {
                record(usage, "Claude", session, &id, to_ms(o.get("timestamp")));
            }
        }
    })
}

// Codex 的 history.jsonl：用户用 $skill 显式调用
fn parse_codex_history(file: &Path, usage: &mut HashMap<String, Record>, r: &Resolver) -> std::io::Result<()> {
    each_line(file, |line| {
        if !line.contains('$') {
            return;
        }
        let Ok(o) = serde_json::from_str::<Value>(line) else { return };
        let Some(session) = o.get("session_id").and_then(|v| v.as_str()) else { return };
        let text = o.get("text").and_then(|v| v.as_str()).unwrap_or("");
        for m in DOLLAR_RE.captures_iter(text) {
            if let Some(id) = r.from_name(&m[1]) {
                record(usage, "Codex", session, &id, to_ms(o.get("ts")));
            }
        }
    })
}

type Parser = fn(&Path, &mut HashMap<String, Record>, &Resolver) -> std::io::Result<()>;

// 文件没变就跳过；变了就整份重读（记录是幂等的）
pub fn update_usage(
    usage: &mut HashMap<String, Record>,
    files: &mut HashMap<String, FileStamp>,
    skills: &[Skill],
    mut on_progress: impl FnMut(usize, usize),
) {
    let r = Resolver::new(skills);
    let mut jobs: Vec<(PathBuf, Parser)> = vec![];
    for f in list_jsonl(&h(".codex/sessions")) {
        jobs.push((f, parse_codex_session));
    }
    for f in list_jsonl(&h(".codex/archived_sessions")) {
        jobs.push((f, parse_codex_session));
    }
    for f in list_jsonl(&h(".claude/projects")) {
        jobs.push((f, parse_claude_session));
    }
    jobs.push((h(".claude/history.jsonl"), parse_claude_history));
    jobs.push((h(".codex/history.jsonl"), parse_codex_history));

    let todo: Vec<_> = jobs
        .into_iter()
        .filter_map(|(f, p)| {
            let st = stamp(&f)?;
            let key = f.to_string_lossy().to_string();
            if files.get(&key) == Some(&st) { None } else { Some((f, p, st)) }
        })
        .collect();
    let total = todo.len();
    for (i, (f, parse, st)) in todo.into_iter().enumerate() {
        on_progress(i, total);
        match parse(&f, usage, &r) {
            Ok(()) => {
                files.insert(f.to_string_lossy().to_string(), st);
            }
            Err(e) => eprintln!("parse failed {}: {e}", f.display()),
        }
    }
    on_progress(total, total);
}

// 汇总成每个 skill 的次数
pub fn summarize(usage: &HashMap<String, Record>) -> HashMap<String, Summary> {
    let mut out: HashMap<String, Summary> = HashMap::new();
    for r in usage.values() {
        let s = out.entry(r.skill.clone()).or_default();
        s.total += 1;
        *s.by_tool.entry(r.tool.clone()).or_default() += 1;
        if r.ts > s.last_used {
            s.last_used = r.ts;
        }
    }
    out
}

fn first_cwd(text: &str) -> Option<String> {
    let m = CWD_RE.captures(text)?;
    serde_json::from_str::<String>(&format!("\"{}\"", &m[1])).ok()
}

// 会话里出现过的工作目录，用于找项目级 skill
pub fn collect_cwds() -> Vec<String> {
    let mut cwds = std::collections::BTreeSet::new();
    for f in list_jsonl(&h(".codex/sessions")) {
        if f.to_string_lossy().ends_with(".zst") {
            continue;
        }
        let Ok(mut file) = File::open(&f) else { continue };
        let mut buf = vec![0u8; 4096];
        let n = file.read(&mut buf).unwrap_or(0);
        if let Some(c) = first_cwd(&String::from_utf8_lossy(&buf[..n])) {
            cwds.insert(c);
        }
    }
    if let Ok(rd) = fs::read_dir(h(".claude/projects")) {
        for d in rd.flatten() {
            if let Some(f) = list_jsonl(&d.path()).into_iter().next() {
                let Ok(mut file) = File::open(&f) else { continue };
                let mut buf = vec![0u8; 60000];
                let n = file.read(&mut buf).unwrap_or(0);
                if let Some(c) = first_cwd(&String::from_utf8_lossy(&buf[..n])) {
                    cwds.insert(c);
                }
            }
        }
    }
    cwds.into_iter().collect()
}
