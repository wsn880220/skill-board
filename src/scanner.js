// 扫描本机所有工具里实际装着的 skill，去重后返回列表
const fs = require('fs');
const path = require('path');
const os = require('os');
const crypto = require('crypto');
const yaml = require('js-yaml');
const TOML = require('smol-toml');

const HOME = os.homedir();
const h = (...p) => path.join(HOME, ...p);

// 不进入的目录：临时目录、备份、插件市场清单、依赖目录
const SKIP_DIR_NAMES = new Set(['node_modules', '.git', 'marketplaces', '.tmp', 'extensions', 'antigravity-backup', 'antigravity-browser-profile']);
const SKIP_PATH_PARTS = ['/.codex/.tmp/', '/marketplaces/', 'plugins-backup', '/node_modules/'];

function exists(p) { try { fs.accessSync(p); return true; } catch { return false; } }
function readJSON(p) { try { return JSON.parse(fs.readFileSync(p, 'utf8')); } catch { return null; } }

// 递归找 SKILL.md，跟随软链接，用 realpath 防循环
function findSkillFiles(root, maxDepth = 8) {
  const out = [];
  const seenDirs = new Set();
  function walk(dir, depth) {
    if (depth > maxDepth) return;
    let real;
    try { real = fs.realpathSync(dir); } catch { return; }
    if (seenDirs.has(real)) return;
    seenDirs.add(real);
    let entries;
    try { entries = fs.readdirSync(dir, { withFileTypes: true }); } catch { return; }
    for (const e of entries) {
      const full = path.join(dir, e.name);
      if (e.name === 'SKILL.md') { out.push(full); continue; }
      if (SKIP_DIR_NAMES.has(e.name)) continue;
      let isDir = e.isDirectory();
      if (e.isSymbolicLink()) { try { isDir = fs.statSync(full).isDirectory(); } catch { isDir = false; } }
      if (isDir) walk(full, depth + 1);
    }
  }
  walk(root, 0);
  return out;
}

// Claude Code：只扫 installed_plugins.json 里登记、且没被停用的插件
function claudePluginRoots() {
  const installed = readJSON(h('.claude/plugins/installed_plugins.json'));
  const settings = readJSON(h('.claude/settings.json')) || {};
  const enabled = settings.enabledPlugins || {};
  const roots = [];
  if (!installed || !installed.plugins) return roots;
  for (const [key, list] of Object.entries(installed.plugins)) {
    if (enabled[key] === false) continue;
    const arr = Array.isArray(list) ? list : [list];
    // 同一插件多条记录时取最近更新的
    const best = arr.filter(x => x && x.installPath).sort((a, b) => String(b.lastUpdated || '').localeCompare(String(a.lastUpdated || '')))[0];
    if (best && exists(best.installPath)) roots.push({ dir: best.installPath, plugin: key });
  }
  return roots;
}

// Codex：plugins/cache/<来源>/<插件>/<版本>/，排除 config.toml 里明确停用的，每个插件只取最新版本
function codexPluginRoots() {
  let cfg = {};
  try { cfg = TOML.parse(fs.readFileSync(h('.codex/config.toml'), 'utf8')); } catch {}
  const pluginCfg = cfg.plugins || {};
  const base = h('.codex/plugins/cache');
  const roots = [];
  let markets = [];
  try { markets = fs.readdirSync(base); } catch { return roots; }
  for (const m of markets) {
    let plugins = [];
    try { plugins = fs.readdirSync(path.join(base, m)); } catch { continue; }
    for (const p of plugins) {
      const key = `${p}@${m}`;
      if (pluginCfg[key] && pluginCfg[key].enabled === false) continue;
      const pdir = path.join(base, m, p);
      let versions = [];
      try {
        versions = fs.readdirSync(pdir, { withFileTypes: true })
          .filter(e => e.isDirectory() && !e.name.startsWith('.'))
          .map(e => ({ dir: path.join(pdir, e.name), mtime: fs.statSync(path.join(pdir, e.name)).mtimeMs }));
      } catch { continue; }
      versions.sort((a, b) => b.mtime - a.mtime);
      if (versions[0]) roots.push({ dir: versions[0].dir, plugin: key });
    }
  }
  return roots;
}

// 所有全局扫描位置：tool 是界面上显示的工具名，source 是默认来源
function globalRoots() {
  const r = [];
  const add = (dir, tool, source, plugin) => { if (exists(dir)) r.push({ dir, tool, source, plugin }); };
  add(h('.claude/skills'), 'Claude', 'user');
  for (const p of claudePluginRoots()) add(p.dir, 'Claude', 'plugin', p.plugin);
  add(h('.codex/skills'), 'Codex', 'user');
  add(h('.codex/vendor_imports'), 'Codex', 'builtin');
  for (const p of codexPluginRoots()) add(p.dir, 'Codex', 'plugin', p.plugin);
  add(h('.agents/skills'), 'Agents', 'user');
  add(h('.cc-switch/skills'), 'cc-switch', 'user');
  add(h('.config/opencode/skills'), 'opencode', 'user');
  add(h('.config/opencode/superpowers'), 'opencode', 'plugin');
  add(h('.config/opencode/plugin'), 'opencode', 'plugin');
  add(h('.cursor/skills-cursor'), 'Cursor', 'builtin');
  add(h('.cursor/skills'), 'Cursor', 'user');
  add(h('.cursor/plugins'), 'Cursor', 'plugin');
  add(h('.gemini/skills'), 'Gemini', 'user');
  add(h('.gemini/antigravity-ide/plugins'), 'Gemini', 'plugin');
  add(h('.gemini/antigravity'), 'Gemini', 'user');
  return r;
}

const PROJECT_SKILL_DIRS = [
  ['.claude/skills', 'Claude'], ['.codex/skills', 'Codex'], ['.agents/skills', 'Agents'],
  ['.cursor/skills', 'Cursor'], ['.opencode/skills', 'opencode'], ['.gemini/skills', 'Gemini'],
];

// 项目级：~/Development 下的项目 + 会话记录里出现过的工作目录
function projectRoots(extraCwds = []) {
  const projects = new Set(extraCwds.filter(d => d && d !== HOME && exists(d)));
  const dev = h('Development');
  // ~/Development 往下最多 3 层找带 .claude/.codex 等目录的项目
  function walk(dir, depth) {
    if (depth > 3) return;
    let entries;
    try { entries = fs.readdirSync(dir, { withFileTypes: true }); } catch { return; }
    if (entries.some(e => ['.claude', '.codex', '.agents', '.cursor', '.opencode', '.gemini'].includes(e.name))) projects.add(dir);
    for (const e of entries) {
      if (e.isDirectory() && !e.name.startsWith('.') && !SKIP_DIR_NAMES.has(e.name)) walk(path.join(dir, e.name), depth + 1);
    }
  }
  walk(dev, 0);
  const r = [];
  for (const p of projects) {
    for (const [sub, tool] of PROJECT_SKILL_DIRS) {
      const d = path.join(p, sub);
      if (exists(d)) r.push({ dir: d, tool, source: 'project', project: p });
    }
  }
  return r;
}

// 读取 frontmatter（兼容 >- 和 | 多行写法）
function parseSkillFile(file) {
  let text;
  try { text = fs.readFileSync(file, 'utf8'); } catch { return null; }
  let meta = {};
  const m = text.match(/^﻿?---\s*\r?\n([\s\S]*?)\r?\n---/);
  if (m) {
    try { meta = yaml.load(m[1]) || {}; } catch {
      // YAML 写得不规范时退回逐行读取
      for (const line of m[1].split(/\r?\n/)) {
        const kv = line.match(/^(name|description):\s*(.*)$/);
        if (kv) meta[kv[1]] = kv[2].replace(/^['"]|['"]$/g, '');
      }
    }
  }
  const dirName = path.basename(path.dirname(file));
  const name = String(meta.name || dirName).trim();
  const description = String(meta.description || '').replace(/\s+/g, ' ').trim();
  return {
    name, dirName, description,
    hash: crypto.createHash('sha1').update(text).digest('hex').slice(0, 16),
    mtime: fs.statSync(file).mtimeMs,
  };
}

const SOURCE_RANK = { user: 0, project: 1, plugin: 2, builtin: 3 };

function scanSkills(extraCwds = []) {
  const roots = [...globalRoots(), ...projectRoots(extraCwds)];
  const byReal = new Map(); // realpath -> 文件信息
  for (const root of roots) {
    for (const f of findSkillFiles(root.dir)) {
      if (SKIP_PATH_PARTS.some(s => f.includes(s))) continue;
      let real;
      try { real = fs.realpathSync(f); } catch { continue; }
      if (SKIP_PATH_PARTS.some(s => real.includes(s))) continue;
      let entry = byReal.get(real);
      if (!entry) {
        const info = parseSkillFile(real);
        if (!info) continue;
        entry = { ...info, real, paths: new Set(), tools: new Set(), sources: new Set(), projects: new Set() };
        byReal.set(real, entry);
      }
      entry.paths.add(f);
      entry.tools.add(root.tool);
      // 路径在插件缓存里一律算插件带的
      let src = root.source;
      if (/\/plugins\/cache\//.test(real)) src = 'plugin';
      if (/\/\.codex\/skills\/\.system\//.test(real)) src = 'builtin';
      entry.sources.add(src);
      if (root.project) entry.projects.add(root.project);
    }
  }

  // 按 name 合并（不区分大小写）
  const skills = new Map();
  for (const e of byReal.values()) {
    const id = e.name.toLowerCase();
    let s = skills.get(id);
    if (!s) {
      s = { id, name: e.name, description: e.description, hash: e.hash, mtime: e.mtime, files: [], paths: [], dirNames: new Set(), tools: new Set(), sources: new Set(), projects: new Set() };
      skills.set(id, s);
    }
    // 描述取最近修改的那份
    if (e.mtime > s.mtime) { s.description = e.description || s.description; s.hash = e.hash; s.mtime = e.mtime; s.name = e.name; }
    s.files.push(e.real);
    s.paths.push(...e.paths);
    s.dirNames.add(e.dirName.toLowerCase());
    e.tools.forEach(t => s.tools.add(t));
    e.sources.forEach(x => s.sources.add(x));
    e.projects.forEach(x => s.projects.add(x));
  }

  return [...skills.values()].map(s => {
    const source = [...s.sources].sort((a, b) => SOURCE_RANK[a] - SOURCE_RANK[b])[0];
    return {
      id: s.id, name: s.name, description: s.description, hash: s.hash,
      files: s.files, paths: [...new Set(s.paths)], dirNames: [...s.dirNames],
      tools: [...s.tools].sort(), source, projects: [...s.projects],
    };
  });
}

module.exports = { scanSkills, HOME };
