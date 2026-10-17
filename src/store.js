// 本地存储：缓存和设置都放在 Electron 的 userData 目录
const fs = require('fs');
const path = require('path');
const { app, safeStorage } = require('electron');

const dir = () => app.getPath('userData');
const file = name => path.join(dir(), name);

function load(name, fallback) {
  try { return JSON.parse(fs.readFileSync(file(name), 'utf8')); } catch { return fallback; }
}
function save(name, data) {
  fs.mkdirSync(dir(), { recursive: true });
  const tmp = file(name) + '.tmp';
  fs.writeFileSync(tmp, JSON.stringify(data));
  fs.renameSync(tmp, file(name));
}

const DEFAULT_SETTINGS = { baseUrl: 'https://api.deepseek.com', model: 'deepseek-chat' };

// API key 用 safeStorage 加密（macOS 下由钥匙串保管密钥）
function loadSettings() {
  const s = { ...DEFAULT_SETTINGS, ...load('settings.json', {}) };
  let apiKey = '';
  if (s.apiKeyEnc) {
    try { apiKey = safeStorage.decryptString(Buffer.from(s.apiKeyEnc, 'base64')); } catch {}
  }
  delete s.apiKeyEnc;
  return { ...s, apiKey };
}
function saveSettings({ baseUrl, model, apiKey, categories }) {
  const out = { baseUrl: baseUrl || DEFAULT_SETTINGS.baseUrl, model: model || DEFAULT_SETTINGS.model };
  if (Array.isArray(categories)) out.categories = [...new Set(categories.map(c => String(c).trim()).filter(Boolean))];
  // apiKey 为 undefined 表示没改，保留原来的
  if (apiKey === undefined) {
    out.apiKeyEnc = load('settings.json', {}).apiKeyEnc || null;
  } else if (apiKey) {
    out.apiKeyEnc = safeStorage.isEncryptionAvailable()
      ? safeStorage.encryptString(apiKey).toString('base64')
      : null;
  }
  save('settings.json', out);
}

module.exports = {
  loadCache: () => load('cache.json', {}),
  saveCache: data => save('cache.json', data),
  loadSettings, saveSettings,
};
