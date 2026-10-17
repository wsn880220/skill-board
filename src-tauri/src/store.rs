// 本地存储：缓存和设置放在应用数据目录，API Key 放在 macOS 钥匙串
use crate::scanner::Skill;
use crate::usage::{FileStamp, Record};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Translation {
    #[serde(default, skip_serializing)]
    pub id: String,
    pub hash: String,
    pub zh: String,
    pub category: String,
    #[serde(default)]
    pub manual: bool,
}

#[derive(Serialize, Deserialize, Default)]
pub struct Cache {
    #[serde(default)]
    pub skills: Vec<Skill>,
    #[serde(default)]
    pub usage: HashMap<String, Record>,
    #[serde(default)]
    pub files: HashMap<String, FileStamp>,
    #[serde(default)]
    pub translations: HashMap<String, Translation>,
    #[serde(default, rename = "scannedAt")]
    pub scanned_at: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Settings {
    #[serde(default = "default_base", rename = "baseUrl")]
    pub base_url: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default)]
    pub categories: Vec<String>,
}
fn default_base() -> String {
    "https://api.deepseek.com".into()
}
fn default_model() -> String {
    "deepseek-chat".into()
}
impl Default for Settings {
    fn default() -> Self {
        Settings { base_url: default_base(), model: default_model(), categories: vec![] }
    }
}

pub struct Store {
    dir: PathBuf,
}

const KEYRING_SERVICE: &str = "Skill Board";
const KEYRING_USER: &str = "deepseek-api-key";

impl Store {
    pub fn new(dir: PathBuf) -> Self {
        let _ = fs::create_dir_all(&dir);
        Store { dir }
    }
    fn load<T: DeserializeOwned + Default>(&self, name: &str) -> T {
        fs::read_to_string(self.dir.join(name)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }
    fn save<T: Serialize>(&self, name: &str, data: &T) {
        let path = self.dir.join(name);
        let tmp = path.with_extension("tmp");
        if let Ok(s) = serde_json::to_string(data) {
            if fs::write(&tmp, s).is_ok() {
                let _ = fs::rename(&tmp, &path);
            }
        }
    }
    pub fn load_cache(&self) -> Cache {
        self.load("cache.json")
    }
    pub fn save_cache(&self, c: &Cache) {
        self.save("cache.json", c)
    }
    pub fn load_settings(&self) -> Settings {
        self.load("settings.json")
    }
    pub fn save_settings(&self, s: &Settings) {
        self.save("settings.json", s)
    }
}

fn entry() -> Option<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER).ok()
}
pub fn load_api_key() -> String {
    entry().and_then(|e| e.get_password().ok()).unwrap_or_default()
}
pub fn save_api_key(key: &str) -> Result<(), String> {
    let e = entry().ok_or("无法访问钥匙串")?;
    if key.is_empty() {
        let _ = e.delete_credential();
        Ok(())
    } else {
        e.set_password(key).map_err(|e| e.to_string())
    }
}
