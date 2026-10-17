// 用 DeepSeek（OpenAI 兼容接口）把 skill 描述翻成中文并归到固定领域
const DEFAULT_CATEGORIES = [
  '浏览器自动化', '飞书/办公协作', '文档处理', '图片生成与处理', '视频与音频',
  '内容写作与发布', '设计与前端', '开发工具与工作流', '知识管理', '数据与可视化',
  'AI/模型接入', '其他',
];
const UNCATEGORIZED = '未分类';
const BATCH = 15;
const CONCURRENCY = 3;

const systemPrompt = categories => `你是一个帮助用户整理 AI 编程助手 skill 的助手。用户会给你一组 skill 的名称和英文（或中英混合）描述。
对每个 skill 输出：
- zh：一两句简洁的中文，说清楚它是干什么的、什么时候用。不要逐字直译，不要复述触发词列表，不超过 60 个汉字。
- category：从下面的领域里选一个最合适的，必须原样使用：${categories.join('、')}。都不合适就选最接近的。
只输出 JSON：{"items":[{"id":"...","zh":"...","category":"..."}]}，id 原样返回。`;

function categoriesOf(settings) {
  const list = (settings.categories || []).map(c => String(c).trim()).filter(Boolean);
  return list.length ? list : DEFAULT_CATEGORIES;
}

async function callBatch(settings, batch) {
  const categories = categoriesOf(settings);
  const body = {
    model: settings.model || 'deepseek-chat',
    response_format: { type: 'json_object' },
    temperature: 0.2,
    messages: [
      { role: 'system', content: systemPrompt(categories) },
      { role: 'user', content: JSON.stringify(batch.map(s => ({ id: s.id, name: s.name, description: (s.description || '').slice(0, 800) }))) },
    ],
  };
  const base = (settings.baseUrl || 'https://api.deepseek.com').replace(/\/+$/, '');
  const res = await fetch(`${base}/chat/completions`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${settings.apiKey}` },
    body: JSON.stringify(body),
    signal: AbortSignal.timeout(120000),
  });
  if (!res.ok) throw new Error(`DeepSeek ${res.status}: ${(await res.text()).slice(0, 200)}`);
  const data = await res.json();
  const parsed = JSON.parse(data.choices[0].message.content);
  return (parsed.items || []).map(x => ({
    id: String(x.id),
    zh: String(x.zh || '').trim(),
    // 模型给出列表外的领域时，归到“其他”（列表里没有“其他”就算未分类）
    category: categories.includes(x.category) ? x.category : (categories.includes('其他') ? '其他' : UNCATEGORIZED),
  }));
}

// 单个 skill 重新生成（编辑弹窗里的“让 AI 重新生成”）
async function translateOne(settings, skill) {
  const items = await callBatch(settings, [skill]);
  if (!items[0]) throw new Error('DeepSeek 没有返回结果');
  return items[0];
}

// store.translations: { [id]: { hash, zh, category, manual } }
// 只处理新增、SKILL.md 内容变了、或所属领域已被删掉的 skill；手动改过的不动
async function translateMissing(store, skills, settings, onProgress = () => {}) {
  store.translations = store.translations || {};
  const categories = categoriesOf(settings);
  const todo = skills.filter(s => {
    const t = store.translations[s.id];
    if (t && t.manual) return false;
    return !t || t.hash !== s.hash || !(categories.includes(t.category) || t.category === UNCATEGORIZED);
  });
  if (!todo.length || !settings.apiKey) return { done: 0, total: todo.length, errors: [] };

  const batches = [];
  for (let i = 0; i < todo.length; i += BATCH) batches.push(todo.slice(i, i + BATCH));
  let done = 0;
  const errors = [];
  onProgress({ phase: 'translate', done, total: todo.length });

  async function run(batch, attempt = 1) {
    try {
      const items = await callBatch(settings, batch);
      const byId = new Map(items.map(x => [x.id, x]));
      for (const s of batch) {
        const x = byId.get(s.id);
        if (x) store.translations[s.id] = { hash: s.hash, zh: x.zh, category: x.category };
      }
    } catch (e) {
      if (attempt < 3) return run(batch, attempt + 1);
      errors.push(e.message);
    }
    done += batch.length;
    onProgress({ phase: 'translate', done, total: todo.length });
  }

  const queue = [...batches];
  await Promise.all(Array.from({ length: CONCURRENCY }, async () => {
    while (queue.length) await run(queue.shift());
  }));
  return { done, total: todo.length, errors };
}

module.exports = { translateMissing, translateOne, categoriesOf, DEFAULT_CATEGORIES, UNCATEGORIZED };
