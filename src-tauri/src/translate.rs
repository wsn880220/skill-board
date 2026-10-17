// 用 DeepSeek（OpenAI 兼容接口）把 skill 描述翻成中文并归到领域
use crate::scanner::Skill;
use crate::store::{Settings, Translation};
use futures::stream::{self, StreamExt};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::time::Duration;

pub const DEFAULT_CATEGORIES: &[&str] = &[
    "浏览器自动化", "飞书/办公协作", "文档处理", "图片生成与处理", "视频与音频",
    "内容写作与发布", "设计与前端", "开发工具与工作流", "知识管理", "数据与可视化",
    "AI/模型接入", "其他",
];
pub const UNCATEGORIZED: &str = "未分类";
const BATCH: usize = 15;
const CONCURRENCY: usize = 3;

fn system_prompt(categories: &[String]) -> String {
    format!(
        "你是一个帮助用户整理 AI 编程助手 skill 的助手。用户会给你一组 skill 的名称和英文（或中英混合）描述。\n\
对每个 skill 输出：\n\
- zh：一两句简洁的中文，说清楚它是干什么的、什么时候用。不要逐字直译，不要复述触发词列表，不超过 60 个汉字。\n\
- category：从下面的领域里选一个最合适的，必须原样使用：{}。都不合适就选最接近的。\n\
只输出 JSON：{{\"items\":[{{\"id\":\"...\",\"zh\":\"...\",\"category\":\"...\"}}]}}，id 原样返回。",
        categories.join("、")
    )
}

pub fn categories_of(settings: &Settings) -> Vec<String> {
    let list: Vec<String> = settings.categories.iter().map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).collect();
    if list.is_empty() { DEFAULT_CATEGORIES.iter().map(|s| s.to_string()).collect() } else { list }
}

#[derive(Deserialize)]
struct Item {
    id: serde_json::Value,
    #[serde(default)]
    zh: String,
    #[serde(default)]
    category: String,
}

pub struct Result1 {
    pub id: String,
    pub zh: String,
    pub category: String,
}

async fn call_batch(client: &reqwest::Client, settings: &Settings, api_key: &str, batch: &[&Skill]) -> Result<Vec<Result1>, String> {
    let categories = categories_of(settings);
    let items: Vec<_> = batch
        .iter()
        .map(|s| json!({ "id": s.id, "name": s.name, "description": s.description.chars().take(800).collect::<String>() }))
        .collect();
    let body = json!({
        "model": if settings.model.is_empty() { "deepseek-chat" } else { &settings.model },
        "response_format": { "type": "json_object" },
        "temperature": 0.2,
        "messages": [
            { "role": "system", "content": system_prompt(&categories) },
            { "role": "user", "content": serde_json::to_string(&items).unwrap() },
        ],
    });
    let base = if settings.base_url.is_empty() { "https://api.deepseek.com" } else { settings.base_url.trim_end_matches('/') };
    let res = client
        .post(format!("{base}/chat/completions"))
        .bearer_auth(api_key)
        .json(&body)
        .timeout(Duration::from_secs(120))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = res.status();
    let text = res.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("DeepSeek {}: {}", status.as_u16(), text.chars().take(200).collect::<String>()));
    }
    let data: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let content = data.pointer("/choices/0/message/content").and_then(|v| v.as_str()).ok_or("DeepSeek 返回格式不对")?;
    #[derive(Deserialize)]
    struct Out {
        #[serde(default)]
        items: Vec<Item>,
    }
    let out: Out = serde_json::from_str(content).map_err(|e| e.to_string())?;
    Ok(out
        .items
        .into_iter()
        .map(|x| Result1 {
            id: match x.id {
                serde_json::Value::String(s) => s,
                v => v.to_string(),
            },
            zh: x.zh.trim().to_string(),
            // 模型给出列表外的领域时，归到“其他”（列表里没有“其他”就算未分类）
            category: if categories.contains(&x.category) {
                x.category
            } else if categories.iter().any(|c| c == "其他") {
                "其他".into()
            } else {
                UNCATEGORIZED.into()
            },
        })
        .collect())
}

// 单个 skill 重新生成（编辑弹窗里的“让 AI 重新生成”）
pub async fn translate_one(settings: &Settings, api_key: &str, skill: &Skill) -> Result<Result1, String> {
    let client = reqwest::Client::new();
    call_batch(&client, settings, api_key, &[skill]).await?.into_iter().next().ok_or_else(|| "DeepSeek 没有返回结果".into())
}

// 需要处理的：新增、SKILL.md 内容变了、或所属领域已被删掉；手动改过的不动
pub fn pending<'a>(skills: &'a [Skill], translations: &HashMap<String, Translation>, settings: &Settings) -> Vec<&'a Skill> {
    let categories = categories_of(settings);
    skills
        .iter()
        .filter(|s| match translations.get(&s.id) {
            Some(t) if t.manual => false,
            Some(t) => t.hash != s.hash || !(categories.contains(&t.category) || t.category == UNCATEGORIZED),
            None => true,
        })
        .collect()
}

// 分批并发处理，每完成一批回调一次（用于落盘和刷新界面）
pub async fn translate_batches(
    todo: Vec<Skill>,
    settings: Settings,
    api_key: String,
    mut on_batch: impl FnMut(Vec<Translation>, Vec<String>, usize, usize),
) -> Vec<String> {
    let client = reqwest::Client::new();
    let total = todo.len();
    let chunks: Vec<Vec<Skill>> = todo.chunks(BATCH).map(|c| c.to_vec()).collect();
    let mut errors = vec![];
    let mut done = 0;
    let mut results = stream::iter(chunks.into_iter().map(|batch| {
        let client = client.clone();
        let settings = settings.clone();
        let api_key = api_key.clone();
        async move {
            let refs: Vec<&Skill> = batch.iter().collect();
            let mut last_err = String::new();
            for _ in 0..3 {
                match call_batch(&client, &settings, &api_key, &refs).await {
                    Ok(items) => {
                        let by_id: HashMap<String, Result1> = items.into_iter().map(|x| (x.id.clone(), x)).collect();
                        let ts: Vec<Translation> = batch
                            .iter()
                            .filter_map(|s| {
                                by_id.get(&s.id).map(|x| Translation {
                                    id: s.id.clone(), hash: s.hash.clone(), zh: x.zh.clone(), category: x.category.clone(), manual: false,
                                })
                            })
                            .collect();
                        return (ts, None, batch.len());
                    }
                    Err(e) => last_err = e,
                }
            }
            (vec![], Some(last_err), batch.len())
        }
    }))
    .buffer_unordered(CONCURRENCY);
    while let Some((ts, err, n)) = results.next().await {
        done += n;
        let errs: Vec<String> = err.into_iter().collect();
        errors.extend(errs.clone());
        on_batch(ts, errs, done, total);
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    // 极简模拟服务：每个请求都返回同一段 items
    fn mock(items: &'static str) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                let mut s = s;
                let mut buf = vec![0u8; 65536];
                let _ = s.read(&mut buf);
                let content = serde_json::to_string(&format!("{{\"items\":{items}}}")).unwrap();
                let body = format!("{{\"choices\":[{{\"message\":{{\"content\":{content}}}}}]}}");
                let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
            }
        });
        format!("http://{addr}")
    }

    fn skill(id: &str, hash: &str) -> Skill {
        Skill { id: id.into(), name: id.into(), description: "d".into(), hash: hash.into(), files: vec![], paths: vec![], dir_names: vec![], tools: vec![], source: "user".into(), projects: vec![] }
    }

    #[test]
    fn category_fallback() {
        let base = mock(r#"[{"id":"a","zh":"中文","category":"瞎编"}]"#);
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let s = Settings { base_url: base.clone(), model: "m".into(), categories: vec![] };
        let r = rt.block_on(translate_one(&s, "k", &skill("a", "h"))).unwrap();
        assert_eq!((r.zh.as_str(), r.category.as_str()), ("中文", "其他"));
        let s2 = Settings { base_url: base, model: "m".into(), categories: vec!["别的".into()] };
        let r = rt.block_on(translate_one(&s2, "k", &skill("a", "h"))).unwrap();
        assert_eq!(r.category, UNCATEGORIZED);
    }

    #[test]
    fn pending_rules() {
        let s = Settings { categories: vec!["我的领域".into()], ..Default::default() };
        let t = |c: &str, manual: bool| Translation { id: String::new(), hash: "h".into(), zh: "z".into(), category: c.into(), manual };
        let mut tr = HashMap::new();
        tr.insert("removed".to_string(), t("旧领域", false));
        tr.insert("manual".to_string(), t("旧领域", true));
        tr.insert("ok".to_string(), t("我的领域", false));
        tr.insert("uncat".to_string(), t(UNCATEGORIZED, false));
        tr.insert("changed".to_string(), Translation { hash: "old".into(), ..t("我的领域", false) });
        let skills: Vec<Skill> = ["removed", "manual", "ok", "uncat", "changed", "new"].iter().map(|id| skill(id, "h")).collect();
        let mut ids: Vec<_> = pending(&skills, &tr, &s).iter().map(|s| s.id.clone()).collect();
        ids.sort();
        assert_eq!(ids, vec!["changed", "new", "removed"]);
    }

    #[test]
    fn batches_all() {
        let items: String = format!("[{}]", (0..40).map(|i| format!(r#"{{"id":"s{i}","zh":"z","category":"其他"}}"#)).collect::<Vec<_>>().join(","));
        let base = mock(Box::leak(items.into_boxed_str()));
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let todo: Vec<Skill> = (0..40).map(|i| skill(&format!("s{i}"), "h")).collect();
        let mut got = 0;
        let errs = rt.block_on(translate_batches(todo, Settings { base_url: base, ..Default::default() }, "k".into(), |ts, _, _, _| got += ts.len()));
        assert!(errs.is_empty());
        assert_eq!(got, 40);
    }
}
