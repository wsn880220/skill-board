// 扫描本机所有工具里实际装着的 skill，去重后返回列表
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

// 不进入的目录：临时目录、备份、插件市场清单、依赖目录
const SKIP_DIR_NAMES: &[&str] = &[
    "node_modules", ".git", "marketplaces", ".tmp", "extensions",
    "antigravity-backup", "antigravity-browser-profile",
];
const SKIP_PATH_PARTS: &[&str] = &["/.codex/.tmp/", "/marketplaces/", "plugins-backup", "/node_modules/"];

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Skill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub hash: String,
    pub files: Vec<String>,
    pub paths: Vec<String>,
    #[serde(rename = "dirNames")]
    pub dir_names: Vec<String>,
    pub tools: Vec<String>,
    pub source: String,
    pub projects: Vec<String>,
}

pub fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}
fn h(p: &str) -> PathBuf {
    home().join(p)
}
fn skipped(p: &str) -> bool {
    SKIP_PATH_PARTS.iter().any(|s| p.contains(s))
}

// 递归找 SKILL.md，跟随软链接，用 realpath 防循环
fn find_skill_files(root: &Path, max_depth: usize) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, max: usize, seen: &mut HashSet<PathBuf>, out: &mut Vec<PathBuf>) {
        if depth > max {
            return;
        }
        let Ok(real) = fs::canonicalize(dir) else { return };
        if !seen.insert(real) {
            return;
        }
        let Ok(entries) = fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let full = e.path();
            if name == "SKILL.md" {
                out.push(full);
                continue;
            }
            if SKIP_DIR_NAMES.contains(&name.as_str()) {
                continue;
            }
            // metadata() 会跟随软链接
            if fs::metadata(&full).map(|m| m.is_dir()).unwrap_or(false) {
                walk(&full, depth + 1, max, seen, out);
            }
        }
    }
    let mut out = vec![];
    walk(root, 0, max_depth, &mut HashSet::new(), &mut out);
    out
}

struct Root {
    dir: PathBuf,
    tool: &'static str,
    source: &'static str,
    project: Option<String>,
}

// Claude Code：只扫 installed_plugins.json 里登记、且没被停用的插件
fn claude_plugin_roots() -> Vec<PathBuf> {
    let read = |p: PathBuf| -> serde_json::Value {
        fs::read_to_string(p).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    };
    let installed = read(h(".claude/plugins/installed_plugins.json"));
    let settings = read(h(".claude/settings.json"));
    let mut roots = vec![];
    let Some(plugins) = installed.get("plugins").and_then(|v| v.as_object()) else { return roots };
    for (key, list) in plugins {
        if settings.pointer(&format!("/enabledPlugins/{}", key.replace('~', "~0").replace('/', "~1"))) == Some(&serde_json::Value::Bool(false)) {
            continue;
        }
        let arr = match list {
            serde_json::Value::Array(a) => a.clone(),
            v => vec![v.clone()],
        };
        // 同一插件多条记录时取最近更新的
        let best = arr
            .iter()
            .filter(|x| x.get("installPath").and_then(|v| v.as_str()).is_some())
            .max_by_key(|x| x.get("lastUpdated").and_then(|v| v.as_str()).unwrap_or("").to_string());
        if let Some(p) = best.and_then(|b| b.get("installPath")).and_then(|v| v.as_str()) {
            if Path::new(p).exists() {
                roots.push(PathBuf::from(p));
            }
        }
    }
    roots
}

// Codex：plugins/cache/<来源>/<插件>/<版本>/，排除 config.toml 里明确停用的，每个插件只取最新版本
fn codex_plugin_roots() -> Vec<PathBuf> {
    let cfg: toml::Value = fs::read_to_string(h(".codex/config.toml"))
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(toml::Value::Table(Default::default()));
    let base = h(".codex/plugins/cache");
    let mut roots = vec![];
    let Ok(markets) = fs::read_dir(&base) else { return roots };
    for m in markets.flatten() {
        let Ok(plugins) = fs::read_dir(m.path()) else { continue };
        for p in plugins.flatten() {
            let key = format!("{}@{}", p.file_name().to_string_lossy(), m.file_name().to_string_lossy());
            let disabled = cfg
                .get("plugins")
                .and_then(|t| t.get(&key))
                .and_then(|t| t.get("enabled"))
                .and_then(|v| v.as_bool())
                == Some(false);
            if disabled {
                continue;
            }
            let Ok(versions) = fs::read_dir(p.path()) else { continue };
            let newest = versions
                .flatten()
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false) && !e.file_name().to_string_lossy().starts_with('.'))
                .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok());
            if let Some(v) = newest {
                roots.push(v.path());
            }
        }
    }
    roots
}

// 所有全局扫描位置：tool 是界面上显示的工具名，source 是默认来源
fn global_roots() -> Vec<Root> {
    let mut r = vec![];
    let mut add = |dir: PathBuf, tool: &'static str, source: &'static str| {
        if dir.exists() {
            r.push(Root { dir, tool, source, project: None });
        }
    };
    add(h(".claude/skills"), "Claude", "user");
    for p in claude_plugin_roots() {
        add(p, "Claude", "plugin");
    }
    add(h(".codex/skills"), "Codex", "user");
    add(h(".codex/vendor_imports"), "Codex", "builtin");
    for p in codex_plugin_roots() {
        add(p, "Codex", "plugin");
    }
    add(h(".agents/skills"), "Agents", "user");
    add(h(".cc-switch/skills"), "cc-switch", "user");
    add(h(".config/opencode/skills"), "opencode", "user");
    add(h(".config/opencode/superpowers"), "opencode", "plugin");
    add(h(".config/opencode/plugin"), "opencode", "plugin");
    add(h(".cursor/skills-cursor"), "Cursor", "builtin");
    add(h(".cursor/skills"), "Cursor", "user");
    add(h(".cursor/plugins"), "Cursor", "plugin");
    add(h(".gemini/skills"), "Gemini", "user");
    add(h(".gemini/antigravity-ide/plugins"), "Gemini", "plugin");
    add(h(".gemini/antigravity"), "Gemini", "user");
    r
}

const PROJECT_SKILL_DIRS: &[(&str, &str)] = &[
    (".claude/skills", "Claude"), (".codex/skills", "Codex"), (".agents/skills", "Agents"),
    (".cursor/skills", "Cursor"), (".opencode/skills", "opencode"), (".gemini/skills", "Gemini"),
];
const PROJECT_MARKERS: &[&str] = &[".claude", ".codex", ".agents", ".cursor", ".opencode", ".gemini"];

// 项目级：~/Development 下的项目 + 会话记录里出现过的工作目录
fn project_roots(extra_cwds: &[String]) -> Vec<Root> {
    let home = home();
    let mut projects: BTreeSet<PathBuf> = extra_cwds
        .iter()
        .map(PathBuf::from)
        .filter(|d| d != &home && d.exists())
        .collect();
    // ~/Development 往下最多 3 层找带 .claude/.codex 等目录的项目
    fn walk(dir: &Path, depth: usize, out: &mut BTreeSet<PathBuf>) {
        if depth > 3 {
            return;
        }
        let Ok(entries) = fs::read_dir(dir) else { return };
        let entries: Vec<_> = entries.flatten().collect();
        if entries.iter().any(|e| PROJECT_MARKERS.contains(&e.file_name().to_string_lossy().as_ref())) {
            out.insert(dir.to_path_buf());
        }
        for e in entries {
            let name = e.file_name().to_string_lossy().to_string();
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) && !name.starts_with('.') && !SKIP_DIR_NAMES.contains(&name.as_str()) {
                walk(&e.path(), depth + 1, out);
            }
        }
    }
    walk(&h("Development"), 0, &mut projects);
    let mut r = vec![];
    for p in projects {
        for (sub, tool) in PROJECT_SKILL_DIRS {
            let d = p.join(sub);
            if d.exists() {
                r.push(Root { dir: d, tool, source: "project", project: Some(p.to_string_lossy().to_string()) });
            }
        }
    }
    r
}

struct FileInfo {
    name: String,
    dir_name: String,
    description: String,
    hash: String,
    mtime: std::time::SystemTime,
}

// 读取 frontmatter（兼容 >- 和 | 多行写法）
fn parse_skill_file(file: &Path) -> Option<FileInfo> {
    let text = fs::read_to_string(file).ok()?;
    let body = text.trim_start_matches('\u{feff}');
    let mut name: Option<String> = None;
    let mut description: Option<String> = None;
    if let Some(rest) = body.strip_prefix("---") {
        let rest = rest.trim_start_matches([' ', '\t']).trim_start_matches('\r').trim_start_matches('\n');
        if let Some(end) = find_frontmatter_end(rest) {
            let fm = &rest[..end];
            match serde_yaml::from_str::<serde_yaml::Value>(fm) {
                Ok(v) => {
                    name = v.get("name").and_then(yaml_str);
                    description = v.get("description").and_then(yaml_str);
                }
                Err(_) => {
                    // YAML 写得不规范时退回逐行读取
                    for line in fm.lines() {
                        if let Some(v) = line.strip_prefix("name:") {
                            name = Some(unquote(v));
                        } else if let Some(v) = line.strip_prefix("description:") {
                            description = Some(unquote(v));
                        }
                    }
                }
            }
        }
    }
    let dir_name = file.parent()?.file_name()?.to_string_lossy().to_string();
    let name = name.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| dir_name.clone());
    let description = description.unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" ");
    let mtime = fs::metadata(file).and_then(|m| m.modified()).ok()?;
    let hash = sha1_smol::Sha1::from(text.as_bytes()).digest().to_string()[..16].to_string();
    Some(FileInfo { name, dir_name, description, hash, mtime })
}

// frontmatter 结束位置：下一行以 --- 开头
fn find_frontmatter_end(s: &str) -> Option<usize> {
    let mut pos = 0;
    for line in s.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']).starts_with("---") {
            return Some(pos);
        }
        pos += line.len();
    }
    None
}
fn yaml_str(v: &serde_yaml::Value) -> Option<String> {
    match v {
        serde_yaml::Value::String(s) => Some(s.clone()),
        serde_yaml::Value::Number(n) => Some(n.to_string()),
        serde_yaml::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}
fn unquote(s: &str) -> String {
    s.trim().trim_matches(|c| c == '\'' || c == '"').to_string()
}

fn source_rank(s: &str) -> u8 {
    match s {
        "user" => 0,
        "project" => 1,
        "plugin" => 2,
        _ => 3,
    }
}

struct Entry {
    info: FileInfo,
    real: String,
    paths: BTreeSet<String>,
    tools: BTreeSet<String>,
    sources: BTreeSet<String>,
    projects: BTreeSet<String>,
}

pub fn scan_skills(extra_cwds: &[String]) -> Vec<Skill> {
    let mut roots = global_roots();
    roots.extend(project_roots(extra_cwds));

    let mut by_real: HashMap<String, Entry> = HashMap::new();
    let mut order: Vec<String> = vec![];
    for root in &roots {
        for f in find_skill_files(&root.dir, 8) {
            let fs_ = f.to_string_lossy().to_string();
            if skipped(&fs_) {
                continue;
            }
            let Ok(real) = fs::canonicalize(&f) else { continue };
            let real = real.to_string_lossy().to_string();
            if skipped(&real) {
                continue;
            }
            if !by_real.contains_key(&real) {
                let Some(info) = parse_skill_file(Path::new(&real)) else { continue };
                order.push(real.clone());
                by_real.insert(real.clone(), Entry {
                    info, real: real.clone(),
                    paths: BTreeSet::new(), tools: BTreeSet::new(), sources: BTreeSet::new(), projects: BTreeSet::new(),
                });
            }
            let e = by_real.get_mut(&real).unwrap();
            e.paths.insert(fs_);
            e.tools.insert(root.tool.to_string());
            // 路径在插件缓存里一律算插件带的
            let mut src = root.source;
            if real.contains("/plugins/cache/") {
                src = "plugin";
            }
            if real.contains("/.codex/skills/.system/") {
                src = "builtin";
            }
            e.sources.insert(src.to_string());
            if let Some(p) = &root.project {
                e.projects.insert(p.clone());
            }
        }
    }

    // 按 name 合并（不区分大小写）
    struct Acc {
        skill: Skill,
        mtime: std::time::SystemTime,
        dir_names: BTreeSet<String>,
        tools: BTreeSet<String>,
        sources: BTreeSet<String>,
        projects: BTreeSet<String>,
        paths: BTreeSet<String>,
    }
    let mut merged: HashMap<String, Acc> = HashMap::new();
    let mut ids: Vec<String> = vec![];
    for real in &order {
        let e = &by_real[real];
        let id = e.info.name.to_lowercase();
        let acc = merged.entry(id.clone()).or_insert_with(|| {
            ids.push(id.clone());
            Acc {
                skill: Skill {
                    id: id.clone(), name: e.info.name.clone(), description: e.info.description.clone(), hash: e.info.hash.clone(),
                    files: vec![], paths: vec![], dir_names: vec![], tools: vec![], source: String::new(), projects: vec![],
                },
                mtime: e.info.mtime,
                dir_names: BTreeSet::new(), tools: BTreeSet::new(), sources: BTreeSet::new(), projects: BTreeSet::new(), paths: BTreeSet::new(),
            }
        });
        // 描述取最近修改的那份
        if e.info.mtime > acc.mtime {
            if !e.info.description.is_empty() {
                acc.skill.description = e.info.description.clone();
            }
            acc.skill.hash = e.info.hash.clone();
            acc.skill.name = e.info.name.clone();
            acc.mtime = e.info.mtime;
        }
        acc.skill.files.push(e.real.clone());
        acc.paths.extend(e.paths.iter().cloned());
        acc.dir_names.insert(e.info.dir_name.to_lowercase());
        acc.tools.extend(e.tools.iter().cloned());
        acc.sources.extend(e.sources.iter().cloned());
        acc.projects.extend(e.projects.iter().cloned());
    }

    ids.into_iter()
        .map(|id| {
            let a = merged.remove(&id).unwrap();
            let mut s = a.skill;
            s.source = a.sources.iter().min_by_key(|x| source_rank(x)).cloned().unwrap_or_default();
            s.paths = a.paths.into_iter().collect();
            s.dir_names = a.dir_names.into_iter().collect();
            s.tools = a.tools.into_iter().collect();
            s.projects = a.projects.into_iter().collect();
            s
        })
        .collect()
}
