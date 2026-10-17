# Skill Board

一个 macOS 桌面小工具，把本机所有 AI 编程助手（Claude Code、Codex、opencode、Cursor、Gemini 等）里装着的 skill 汇总成一张**使用排行榜**：哪些天天在用，哪些装了就忘了，一眼就能看出来。

## 功能

- **自动扫描**全局和项目级的 skill，跟随软链接去重；跳过临时目录、备份和插件市场清单，已停用的插件也不算在内。
- **统计使用次数**：从各工具现有的会话日志中统计（Codex 会话，包括压缩的 `.jsonl.zst`；Claude Code 会话和 `history.jsonl`），计数单位是“用过这个 skill 的会话数”。统计结果会累计保存，原始日志被清理后次数也不会减少。
- **中文描述 + 领域分类**：调用 DeepSeek 把描述翻成一两句中文，并归入领域。只处理新出现或改动过的 skill。
- 左侧领域筛选、搜索、“使用最多 / 使用最少”切换。
- 点击任意一行，可以手动修改描述和分类，或者让 AI 重新生成。
- 领域列表可以自定义。
- 不联网也能用：没配置 key 时显示英文原描述。

## 运行

基于 [Tauri 2](https://tauri.app)：界面是 `renderer/` 下的静态网页，后台逻辑在 `src-tauri/`（Rust）。安装包约 3MB。

需要 Rust（rustup）和 Node.js（只用来运行 Tauri CLI）。

```bash
npm install
npm run dev        # 开发模式运行
npm run build      # 打包，产物在 src-tauri/target/release/bundle/
```

打开应用后，点右上角齿轮填入 DeepSeek API Key，保存后会自动开始翻译和分类。接口地址和模型也可以改成其他 OpenAI 兼容的服务。

## 数据与隐私

- 所有数据只在本机读取。缓存和设置保存在 `~/Library/Application Support/io.github.wsn880220.skillboard/`。
- API Key 保存在 macOS 钥匙串里。
- 只有 skill 的名称和描述会发送给你配置的模型服务，会话内容不会外发。

## 已知限制

- opencode、Cursor、Gemini 的使用记录尚未接入，这些工具目前贡献 0 次。
- 项目级 skill 只扫描 `~/Development` 下的项目，以及会话记录中出现过的工作目录。
- 应用未签名，首次打开可能需要右键选择“打开”。

## License

MIT
