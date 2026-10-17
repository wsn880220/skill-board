// 从现有会话日志里统计 skill 使用情况
// 计数单位是“会话 × skill”；解析结果累计保存在 store.usage，原始日志被删也不丢
const fs = require('fs');
const path = require('path');
const readline = require('readline');
const zlib = require('zlib');
const { HOME } = require('./scanner');

const h = (...p) => path.join(HOME, ...p);
const SKILL_PATH_RE = /((?:~|\/)[^\s"'\\`]*?\/)?([^\s"'\\`/]+)\/SKILL\.md/g;
const CODEX_CALL_TYPES = new Set(['function_call', 'custom_tool_call', 'local_shell_call']);

function listJsonl(dir) {
  const out = [];
  (function walk(d) {
    let entries;
    try { entries = fs.readdirSync(d, { withFileTypes: true }); } catch { return; }
    for (const e of entries) {
      const full = path.join(d, e.name);
      if (e.isDirectory()) walk(full);
      else if (e.name.endsWith('.jsonl') || e.name.endsWith('.jsonl.zst')) out.push(full);
    }
  })(dir);
  return out;
}

async function eachLine(file, fn) {
  // 旧的 Codex 会话会被压缩成 .jsonl.zst
  let input = fs.createReadStream(file);
  if (file.endsWith('.zst')) input = input.pipe(zlib.createZstdDecompress());
  input.setEncoding('utf8');
  const rl = readline.createInterface({ input, crlfDelay: Infinity });
  for await (const line of rl) fn(line);
}

// 把日志里出现的 skill 引用（路径或名字）归到扫描出来的某个 skill 上
function makeResolver(skills) {
  const byFile = new Map(), byDir = new Map(), byName = new Map();
  for (const s of skills) {
    for (const f of s.files) byFile.set(path.dirname(f), s.id);
    for (const p of s.paths) byFile.set(path.dirname(p), s.id);
    for (const d of s.dirNames) if (!byDir.has(d)) byDir.set(d, s.id);
    byName.set(s.id, s.id);
  }
  const realCache = new Map();
  function fromPath(prefix, dirName) {
    if (prefix) {
      const full = path.join(prefix.replace(/^~/, HOME), dirName);
      if (byFile.has(full)) return byFile.get(full);
      if (!realCache.has(full)) { let r = null; try { r = fs.realpathSync(full); } catch {} realCache.set(full, r); }
      const real = realCache.get(full);
      if (real && byFile.has(real)) return byFile.get(real);
    }
    const d = dirName.toLowerCase();
    return byDir.get(d) || byName.get(d) || null;
  }
  function fromName(name) {
    const n = String(name).toLowerCase().replace(/^.*:/, '');
    return byName.get(n) || byDir.get(n) || null;
  }
  function fromText(text) {
    const ids = new Set();
    for (const m of text.matchAll(SKILL_PATH_RE)) {
      const id = fromPath(m[1], m[2]);
      if (id) ids.add(id);
    }
    return ids;
  }
  return { fromName, fromText };
}

function record(store, tool, session, skillId, ts) {
  const key = `${tool}|${session}|${skillId}`;
  const prev = store.usage[key];
  if (!prev || (ts && ts > prev.ts)) store.usage[key] = { tool, session, skill: skillId, ts: ts || (prev && prev.ts) || 0 };
}

function toMs(t) {
  if (!t) return 0;
  if (typeof t === 'number') return t < 1e12 ? t * 1000 : t;
  const v = Date.parse(t);
  return isNaN(v) ? 0 : v;
}

// 文件没变就跳过；变了就整份重读（记录是幂等的）
function changed(store, file) {
  let st;
  try { st = fs.statSync(file); } catch { return false; }
  const prev = store.files[file];
  if (prev && prev.size === st.size && prev.mtime === st.mtimeMs) return false;
  return st;
}

async function parseCodexSession(file, store, resolve) {
  let session = path.basename(file).replace(/\.jsonl(\.zst)?$/, '');
  await eachLine(file, line => {
    if (line.includes('"session_meta"')) {
      try { const o = JSON.parse(line); session = o.payload.session_id || o.payload.id || session; } catch {}
      return;
    }
    if (!line.includes('SKILL.md')) return;
    let o;
    try { o = JSON.parse(line); } catch { return; }
    if (o.type !== 'response_item' || !o.payload || !CODEX_CALL_TYPES.has(o.payload.type)) return;
    const ids = resolve.fromText(JSON.stringify(o.payload));
    for (const id of ids) record(store, 'Codex', session, id, toMs(o.timestamp));
  });
}

async function parseClaudeSession(file, store, resolve) {
  await eachLine(file, line => {
    const hasSkill = line.includes('"Skill"');
    const hasMd = line.includes('SKILL.md');
    const hasCmd = line.includes('<command-name>');
    if (!hasSkill && !hasMd && !hasCmd) return;
    let o;
    try { o = JSON.parse(line); } catch { return; }
    const session = o.sessionId || path.basename(file, '.jsonl');
    const ts = toMs(o.timestamp);
    const content = o.message && Array.isArray(o.message.content) ? o.message.content : [];
    if (o.type === 'assistant') {
      for (const c of content) {
        if (c.type !== 'tool_use') continue;
        if (c.name === 'Skill' && c.input && c.input.skill) {
          const id = resolve.fromName(c.input.skill);
          if (id) record(store, 'Claude', session, id, ts);
        } else if (c.input) {
          for (const id of resolve.fromText(JSON.stringify(c.input))) record(store, 'Claude', session, id, ts);
        }
      }
    } else if (o.type === 'user' && hasCmd) {
      const text = typeof o.message?.content === 'string' ? o.message.content : JSON.stringify(o.message?.content || '');
      for (const m of text.matchAll(/<command-name>\/?([^<\s]+)<\/command-name>/g)) {
        const id = resolve.fromName(m[1]);
        if (id) record(store, 'Claude', session, id, ts);
      }
    }
  });
}

// Claude 的 history.jsonl：用户手动敲的 /命令
async function parseClaudeHistory(file, store, resolve) {
  await eachLine(file, line => {
    let o;
    try { o = JSON.parse(line); } catch { return; }
    const m = String(o.display || '').match(/^\/([\w:.-]+)/);
    if (!m || !o.sessionId) return;
    const id = resolve.fromName(m[1]);
    if (id) record(store, 'Claude', o.sessionId, id, toMs(o.timestamp));
  });
}

// Codex 的 history.jsonl：用户用 $skill 显式调用
async function parseCodexHistory(file, store, resolve) {
  await eachLine(file, line => {
    if (!line.includes('$')) return;
    let o;
    try { o = JSON.parse(line); } catch { return; }
    if (!o.session_id) return;
    for (const m of String(o.text || '').matchAll(/(?:^|\s)\$([A-Za-z][\w:.-]*)/g)) {
      const id = resolve.fromName(m[1]);
      if (id) record(store, 'Codex', o.session_id, id, toMs(o.ts));
    }
  });
}

async function updateUsage(store, skills, onProgress = () => {}) {
  store.usage = store.usage || {};
  store.files = store.files || {};
  const resolve = makeResolver(skills);
  const jobs = [
    ...listJsonl(h('.codex/sessions')).map(f => [f, parseCodexSession]),
    ...listJsonl(h('.codex/archived_sessions')).map(f => [f, parseCodexSession]),
    ...listJsonl(h('.claude/projects')).map(f => [f, parseClaudeSession]),
    [h('.claude/history.jsonl'), parseClaudeHistory],
    [h('.codex/history.jsonl'), parseCodexHistory],
  ];
  const todo = [];
  for (const [f, fn] of jobs) {
    const st = changed(store, f);
    if (st) todo.push([f, fn, st]);
  }
  let done = 0;
  for (const [f, fn, st] of todo) {
    onProgress({ phase: 'usage', done, total: todo.length });
    try {
      await fn(f, store, resolve);
      store.files[f] = { size: st.size, mtime: st.mtimeMs };
    } catch (e) { console.error('parse failed', f, e.message); }
    done++;
  }
  onProgress({ phase: 'usage', done, total: todo.length });
}

// 汇总成每个 skill 的次数
function summarize(store) {
  const out = {};
  for (const r of Object.values(store.usage || {})) {
    const s = out[r.skill] || (out[r.skill] = { total: 0, byTool: {}, lastUsed: 0 });
    s.total++;
    s.byTool[r.tool] = (s.byTool[r.tool] || 0) + 1;
    if (r.ts > s.lastUsed) s.lastUsed = r.ts;
  }
  return out;
}

// 会话里出现过的工作目录，用于找项目级 skill
async function collectCwds() {
  const cwds = new Set();
  for (const f of listJsonl(h('.codex/sessions'))) {
    try {
      const fd = fs.openSync(f, 'r');
      const buf = Buffer.alloc(4096);
      const n = fs.readSync(fd, buf, 0, 4096, 0);
      fs.closeSync(fd);
      const m = buf.slice(0, n).toString('utf8').match(/"cwd":"((?:[^"\\]|\\.)*)"/);
      if (m) cwds.add(JSON.parse(`"${m[1]}"`));
    } catch {}
  }
  try {
    for (const d of fs.readdirSync(h('.claude/projects'))) {
      for (const f of listJsonl(h('.claude/projects', d)).slice(0, 1)) {
        const m = fs.readFileSync(f, 'utf8').slice(0, 20000).match(/"cwd":"((?:[^"\\]|\\.)*)"/);
        if (m) cwds.add(JSON.parse(`"${m[1]}"`));
      }
    }
  } catch {}
  return [...cwds];
}

module.exports = { updateUsage, summarize, collectCwds };
