const { app, BrowserWindow, ipcMain, nativeTheme } = require('electron');
const path = require('path');
const { scanSkills } = require('./src/scanner');
const { updateUsage, summarize, collectCwds } = require('./src/usage');
const { translateMissing, translateOne, categoriesOf, UNCATEGORIZED } = require('./src/translate');
const store = require('./src/store');

let win;
let cache;            // { skills, usage, files, translations, scannedAt }
let busy = false;
let status = { phase: 'idle' };

function setStatus(s) {
  status = s;
  if (win && !win.isDestroyed()) win.webContents.send('status', s);
}

function buildView() {
  const settings = store.loadSettings();
  const categories = categoriesOf(settings);
  const sum = summarize(cache);
  const tr = cache.translations || {};
  const rows = (cache.skills || []).map(s => {
    const t = tr[s.id];
    const u = sum[s.id] || { total: 0, byTool: {}, lastUsed: 0 };
    return {
      id: s.id, name: s.name, description: s.description,
      zh: t && t.zh ? t.zh : '',
      // 领域已被删掉的（比如手动改过的条目）显示为未分类
      category: t && categories.includes(t.category) ? t.category : UNCATEGORIZED,
      manual: !!(t && t.manual),
      count: u.total, byTool: u.byTool, lastUsed: u.lastUsed,
      tools: s.tools, source: s.source,
    };
  });
  return { rows, categories: [...categories, UNCATEGORIZED], scannedAt: cache.scannedAt || 0, hasKey: !!settings.apiKey };
}

function pushView() {
  if (win && !win.isDestroyed()) win.webContents.send('view', buildView());
}

async function translate() {
  const settings = store.loadSettings();
  if (!settings.apiKey) return;
  const r = await translateMissing(cache, cache.skills, settings, p => {
    setStatus(p);
    // 每完成一批就落盘并刷新界面
    store.saveCache(cache);
    pushView();
  });
  store.saveCache(cache);
  if (r.errors.length) setStatus({ phase: 'error', message: `翻译有 ${r.errors.length} 批失败：${r.errors[0]}` });
}

async function refresh() {
  if (busy) return;
  busy = true;
  try {
    setStatus({ phase: 'scan' });
    const cwds = await collectCwds();
    cache.skills = scanSkills(cwds);
    cache.scannedAt = Date.now();
    pushView();
    await updateUsage(cache, cache.skills, setStatus);
    store.saveCache(cache);
    pushView();
    await translate();
    pushView();
    if (status.phase !== 'error') setStatus({ phase: 'idle' });
  } catch (e) {
    console.error(e);
    setStatus({ phase: 'error', message: e.message });
  } finally {
    busy = false;
  }
}

function createWindow() {
  win = new BrowserWindow({
    width: 1280, height: 820, minWidth: 900, minHeight: 560,
    title: 'Skill Board',
    titleBarStyle: 'hiddenInset',
    backgroundColor: nativeTheme.shouldUseDarkColors ? '#1c1c1e' : '#ffffff',
    webPreferences: { preload: path.join(__dirname, 'preload.js'), contextIsolation: true, nodeIntegration: false },
  });
  win.loadFile(path.join(__dirname, 'renderer/index.html'));
  win.webContents.on('did-finish-load', () => { pushView(); setStatus(status); });
}

ipcMain.handle('rescan', () => refresh());
ipcMain.handle('get-settings', () => {
  const s = store.loadSettings();
  return { baseUrl: s.baseUrl, model: s.model, hasKey: !!s.apiKey, categories: categoriesOf(s) };
});
// 编辑弹窗：让 AI 重新生成单个 skill，只返回结果，保存由用户决定
ipcMain.handle('regenerate', async (_e, id) => {
  const settings = store.loadSettings();
  if (!settings.apiKey) throw new Error('还没有配置 DeepSeek API Key');
  const skill = (cache.skills || []).find(s => s.id === id);
  if (!skill) throw new Error('找不到这个 skill');
  const r = await translateOne(settings, skill);
  return { zh: r.zh, category: r.category };
});
// 编辑弹窗：保存手动修改，之后自动翻译不再覆盖
ipcMain.handle('save-skill', (_e, { id, zh, category }) => {
  const skill = (cache.skills || []).find(s => s.id === id);
  if (!skill) return;
  cache.translations = cache.translations || {};
  cache.translations[id] = { hash: skill.hash, zh: String(zh || '').trim(), category, manual: true };
  store.saveCache(cache);
  pushView();
});
ipcMain.handle('save-settings', async (_e, s) => {
  store.saveSettings(s);
  pushView();
  if (!busy) {
    busy = true;
    try { await translate(); pushView(); if (status.phase !== 'error') setStatus({ phase: 'idle' }); }
    finally { busy = false; }
  }
});

app.whenReady().then(() => {
  cache = store.loadCache();
  createWindow();
  refresh();
  app.on('activate', () => { if (BrowserWindow.getAllWindows().length === 0) createWindow(); });
});

app.on('window-all-closed', () => app.quit());
