mod scanner;
mod store;
mod translate;
mod usage;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use store::{Cache, Settings, Store, Translation};
use tauri::{AppHandle, Emitter, Manager, State};
use translate::{categories_of, UNCATEGORIZED};

struct AppState {
    store: Store,
    cache: Mutex<Cache>,
    busy: AtomicBool,
    status: Mutex<Value>,
}
type S = Arc<AppState>;

#[derive(Serialize, Clone)]
struct Row {
    id: String,
    name: String,
    description: String,
    zh: String,
    category: String,
    manual: bool,
    count: u32,
    #[serde(rename = "byTool")]
    by_tool: HashMap<String, u32>,
    #[serde(rename = "lastUsed")]
    last_used: i64,
    tools: Vec<String>,
    source: String,
}

#[derive(Serialize, Clone)]
struct View {
    rows: Vec<Row>,
    categories: Vec<String>,
    #[serde(rename = "scannedAt")]
    scanned_at: i64,
    #[serde(rename = "hasKey")]
    has_key: bool,
}

fn build_view(st: &AppState) -> View {
    let settings = st.store.load_settings();
    let categories = categories_of(&settings);
    let cache = st.cache.lock().unwrap();
    let sum = usage::summarize(&cache.usage);
    let rows = cache
        .skills
        .iter()
        .map(|s| {
            let t = cache.translations.get(&s.id);
            let u = sum.get(&s.id).cloned().unwrap_or_default();
            Row {
                id: s.id.clone(),
                name: s.name.clone(),
                description: s.description.clone(),
                zh: t.map(|t| t.zh.clone()).unwrap_or_default(),
                // 领域已被删掉的（比如手动改过的条目）显示为未分类
                category: t.filter(|t| categories.contains(&t.category)).map(|t| t.category.clone()).unwrap_or_else(|| UNCATEGORIZED.into()),
                manual: t.map(|t| t.manual).unwrap_or(false),
                count: u.total,
                by_tool: u.by_tool,
                last_used: u.last_used,
                tools: s.tools.clone(),
                source: s.source.clone(),
            }
        })
        .collect();
    let mut cats = categories;
    cats.push(UNCATEGORIZED.into());
    View { rows, categories: cats, scanned_at: cache.scanned_at, has_key: !store::load_api_key().is_empty() }
}

fn push_view(app: &AppHandle, st: &AppState) {
    let _ = app.emit("view", build_view(st));
}

fn set_status(app: &AppHandle, st: &AppState, s: Value) {
    *st.status.lock().unwrap() = s.clone();
    let _ = app.emit("status", s);
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

async fn translate_pending(app: &AppHandle, st: &S) {
    let api_key = store::load_api_key();
    if api_key.is_empty() {
        return;
    }
    let settings = st.store.load_settings();
    let todo: Vec<_> = {
        let cache = st.cache.lock().unwrap();
        translate::pending(&cache.skills, &cache.translations, &settings).into_iter().cloned().collect()
    };
    if todo.is_empty() {
        return;
    }
    set_status(app, st, json!({ "phase": "translate", "done": 0, "total": todo.len() }));
    let app2 = app.clone();
    let st2 = st.clone();
    let errors = translate::translate_batches(todo, settings, api_key, move |ts, _errs, done, total| {
        {
            let mut cache = st2.cache.lock().unwrap();
            for t in ts {
                cache.translations.insert(t.id.clone(), t);
            }
            // 每完成一批就落盘并刷新界面
            st2.store.save_cache(&cache);
        }
        set_status(&app2, &st2, json!({ "phase": "translate", "done": done, "total": total }));
        push_view(&app2, &st2);
    })
    .await;
    if let Some(e) = errors.first() {
        set_status(app, st, json!({ "phase": "error", "message": format!("翻译有 {} 批失败：{}", errors.len(), e) }));
    }
}

fn finish(app: &AppHandle, st: &S) {
    push_view(app, st);
    let is_err = st.status.lock().unwrap().get("phase").and_then(|v| v.as_str()) == Some("error");
    if !is_err {
        set_status(app, st, json!({ "phase": "idle" }));
    }
}

async fn refresh(app: AppHandle, st: S) {
    if st.busy.swap(true, Ordering::SeqCst) {
        return;
    }
    set_status(&app, &st, json!({ "phase": "scan" }));
    let st2 = st.clone();
    let app2 = app.clone();
    // 扫描和读日志是阻塞操作，放到后台线程
    let _ = tauri::async_runtime::spawn_blocking(move || {
        let cwds = usage::collect_cwds();
        let skills = scanner::scan_skills(&cwds);
        let (mut usage_map, mut files) = {
            let mut cache = st2.cache.lock().unwrap();
            cache.skills = skills.clone();
            cache.scanned_at = now_ms();
            // 在副本上读日志，期间界面照常显示旧的统计
            (cache.usage.clone(), cache.files.clone())
        };
        push_view(&app2, &st2);
        usage::update_usage(&mut usage_map, &mut files, &skills, |done, total| {
            set_status(&app2, &st2, json!({ "phase": "usage", "done": done, "total": total }));
        });
        let mut cache = st2.cache.lock().unwrap();
        cache.usage = usage_map;
        cache.files = files;
        st2.store.save_cache(&cache);
    })
    .await;
    push_view(&app, &st);
    translate_pending(&app, &st).await;
    finish(&app, &st);
    st.busy.store(false, Ordering::SeqCst);
}

#[tauri::command]
fn get_view(st: State<S>) -> View {
    build_view(&st)
}

#[tauri::command]
fn get_status(st: State<S>) -> Value {
    st.status.lock().unwrap().clone()
}

#[tauri::command]
fn rescan(app: AppHandle, st: State<S>) {
    let st = st.inner().clone();
    tauri::async_runtime::spawn(refresh(app, st));
}

#[tauri::command]
fn get_settings(st: State<S>) -> Value {
    let s = st.store.load_settings();
    json!({ "baseUrl": s.base_url, "model": s.model, "hasKey": !store::load_api_key().is_empty(), "categories": categories_of(&s) })
}

#[derive(Deserialize)]
struct SettingsInput {
    #[serde(rename = "baseUrl")]
    base_url: String,
    model: String,
    // 没传表示没改
    #[serde(rename = "apiKey")]
    api_key: Option<String>,
    categories: Vec<String>,
}

#[tauri::command]
async fn save_settings(app: AppHandle, st: State<'_, S>, s: SettingsInput) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    let categories = s.categories.into_iter().map(|c| c.trim().to_string()).filter(|c| !c.is_empty() && seen.insert(c.clone())).collect();
    st.store.save_settings(&Settings {
        base_url: if s.base_url.trim().is_empty() { "https://api.deepseek.com".into() } else { s.base_url.trim().into() },
        model: if s.model.trim().is_empty() { "deepseek-chat".into() } else { s.model.trim().into() },
        categories,
    });
    if let Some(k) = s.api_key {
        store::save_api_key(k.trim())?;
    }
    let st = st.inner().clone();
    push_view(&app, &st);
    if !st.busy.swap(true, Ordering::SeqCst) {
        translate_pending(&app, &st).await;
        finish(&app, &st);
        st.busy.store(false, Ordering::SeqCst);
    }
    Ok(())
}

// 编辑弹窗：让 AI 重新生成单个 skill，只返回结果，保存由用户决定
#[tauri::command]
async fn regenerate(st: State<'_, S>, id: String) -> Result<Value, String> {
    let api_key = store::load_api_key();
    if api_key.is_empty() {
        return Err("还没有配置 DeepSeek API Key".into());
    }
    let skill = st.cache.lock().unwrap().skills.iter().find(|s| s.id == id).cloned().ok_or("找不到这个 skill")?;
    let r = translate::translate_one(&st.store.load_settings(), &api_key, &skill).await?;
    Ok(json!({ "zh": r.zh, "category": r.category }))
}

// 编辑弹窗：保存手动修改，之后自动翻译不再覆盖
#[tauri::command]
fn save_skill(app: AppHandle, st: State<S>, id: String, zh: String, category: String) {
    {
        let mut cache = st.cache.lock().unwrap();
        let Some(hash) = cache.skills.iter().find(|s| s.id == id).map(|s| s.hash.clone()) else { return };
        cache.translations.insert(id.clone(), Translation { id, hash, zh: zh.trim().into(), category, manual: true });
        st.store.save_cache(&cache);
    }
    push_view(&app, &st);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            let store = Store::new(dir);
            let cache = store.load_cache();
            let st: S = Arc::new(AppState { store, cache: Mutex::new(cache), busy: AtomicBool::new(false), status: Mutex::new(json!({ "phase": "idle" })) });
            app.manage(st.clone());
            tauri::async_runtime::spawn(refresh(app.handle().clone(), st));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![get_view, get_status, rescan, get_settings, save_settings, regenerate, save_skill])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

// 命令行自检：输出扫描和统计结果，用于和旧版对比
pub fn selftest() {
    let cwds = usage::collect_cwds();
    let skills = scanner::scan_skills(&cwds);
    let mut u = HashMap::new();
    let mut f = HashMap::new();
    let t = std::time::Instant::now();
    usage::update_usage(&mut u, &mut f, &skills, |_, _| {});
    let sum = usage::summarize(&u);
    let mut list: Vec<_> = skills.iter().map(|s| json!({ "id": s.id, "tools": s.tools, "source": s.source, "hash": s.hash, "desc": s.description })).collect();
    list.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    let usage: HashMap<_, _> = sum.iter().map(|(k, v)| (k.clone(), v.total)).collect();
    eprintln!("usage parse took {:?}", t.elapsed());
    println!("{}", json!({ "skills": list, "usage": usage, "records": u.len(), "cwds": cwds }));
}
