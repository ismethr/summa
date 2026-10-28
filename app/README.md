# Summa 开发指南

桌面应用：Tauri 2 + React 18 + TypeScript + Vite 5。界面是 Web 技术，改了立刻生效；文件读写、网络请求和 API Key 都在 Rust 侧。

使用说明见 [使用手册](INSTALL.md)，版本变化见 [更新日志](../CHANGELOG.md)。

- [跑起来](#跑起来)
- [架构](#架构)
- [代码地图](#代码地图)
- [数据模型](#数据模型)
- [Rust 命令](#rust-命令)
- [解析流程](#解析流程)
- [外部导入（收件箱）](#外部导入收件箱)
- [主题与样式](#主题与样式)
- [打包发布](#打包发布)
- [约定与踩过的坑](#约定与踩过的坑)
- [下一步](#下一步)

---

## 跑起来

环境：Node 22、Rust 1.99（含 cargo）、Xcode 命令行工具。

```bash
cd app
npm install        # 只需第一次
npm start          # = tauri dev，打开桌面窗口
```

第一次会编译 Rust，约 10 分钟；之后几秒启动。

| 改了什么 | 怎么生效 |
|---|---|
| `src/*.tsx`、`src/*.ts`、`src/styles.css` | 保存即热更新 |
| `src-tauri/src/*.rs`、`Cargo.toml` | `Ctrl+C` 后重新 `npm start` |
| `tauri.conf.json`、`capabilities/*.json` | 同上 |

**只改界面时**可以用浏览器预览，不必等 Rust 编译：

```bash
npm run dev        # 浏览器打开 http://localhost:1420
```

检测不到 Tauri 环境时，界面用 `src/sample.json` 的样例数据渲染，标题栏显示「浏览器预览模式」。拖入解析、设置、删除、打开 PDF 在这个模式下不可用。

**测试**：

```bash
cd src-tauri && cargo test --lib     # 模型输出容错、外部导入字段转换、合并规则
npm run build                        # 前端类型检查 + 构建
```

## 架构

```
┌──────────────────────── 界面（WebView） ────────────────────────┐
│ App.tsx   三栏布局、筛选排序、多选、拖入队列、收件箱轮询           │
│ parse.ts  PDF.js 把 PDF 渲染成 JPEG                              │
└──────────────┬───────────────────────────────────────────────────┘
               │ invoke("命令", 参数)
┌──────────────▼──────────────── Rust（lib.rs） ───────────────────┐
│ 文献库读写（全局锁 + 原子写）  模型调用（Key 只在这里）            │
│ PDF 副本管理  收件箱导入  设置读写                                 │
└──────┬─────────────────────┬────────────────────────┬────────────┘
       │                     │                        │
  library.json          DeepSeek API              inbox/*.json
  files/*.pdf                                      ↑
  config.json                          skills/summa-import/scripts/summa.py
                                       （WorkBuddy / Claude Code 调用）
```

几条关键决策：

- **PDF 渲染在前端**：PDF.js 是现成的浏览器能力；Rust 侧转图要带 pdfium 动态库，跨平台分发麻烦。
- **模型调用在 Rust**：API Key 不能进前端代码。
- **只有应用写 `library.json`**：外部工具只往 `inbox/` 放文件，由应用收进库。避免两个进程抢着写坏文件。
- **所有写操作经过 `LIBRARY_LOCK`**：连续拖入多篇、轮询导入和界面编辑可能同时发生，不加锁会互相覆盖。
- **写文件先写 `.tmp` 再改名**：写到一半崩溃不会损坏原文件。

## 代码地图

```
app/
├── src/
│   ├── App.tsx          主界面：标题栏、侧栏、列表、筛选排序、多选、拖入队列、收件箱轮询、快捷键
│   ├── Detail.tsx       要义页：星标、已读、复制引用、主题编辑、笔记自动保存
│   ├── BulkPanel.tsx    多选后的批量操作面板
│   ├── Settings.tsx     设置窗口：外观、API Key、模型、测试连接
│   ├── theme.ts         日间 / 夜间 / 跟随系统，同步窗口外观
│   ├── parse.ts         PDF.js 渲染；改页面宽度、JPEG 质量在这里
│   ├── types.ts         前端数据类型，与 Rust 的 Paper 对应
│   ├── styles.css       全部样式，顶部是两套颜色变量
│   └── sample.json      浏览器预览用的样例
│
└── src-tauri/
    ├── src/lib.rs       ★ 数据结构、命令、模型调用、导入、测试
    ├── src/sync.rs      WebDAV 同步：合并规则、HTTP、PDF 传输
    ├── tauri.conf.json  窗口、打包、版本号
    ├── capabilities/    前端可调用的系统权限
    ├── Cargo.toml       Rust 依赖
    └── icons/           应用图标（由 assets/icon-1024.png 生成）
```

## 数据模型

数据目录：macOS `~/Library/Application Support/app.summa.desktop/`，Windows `%APPDATA%\app.summa.desktop\`。

| 文件 | 内容 | 谁写 |
|---|---|---|
| `library.json` | `{ "papers": [Paper, ...], "tombstones": [...] }`，新的在前 | 只有应用 |
| `files/<id>.pdf` | 解析或导入时复制进来的 PDF | 只有应用 |
| `config.json` | `{ "api_key": "...", "model": "...", "webdav": {...} }` | 应用设置窗口 |
| `sync-state.json` | 上次同步的 ETag、内容指纹、时间 | 同步命令 |
| `inbox/*.json` | 外部工具提交的待导入数据 | `summa.py` |
| `inbox/failed/` | 格式有误的文件和对应的 `.error.txt` | 应用 |

`Paper` 字段（Rust `lib.rs` 与 `src/types.ts` 一一对应，改字段要两边一起改）：

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | string | `p` + 毫秒时间戳 |
| `title` `authors` `venue` | string | `authors` 是逗号分隔的字符串 |
| `year` | number | 未知为 0 |
| `tags` | string[] | 主题 |
| `gist` | string | 一句话要旨 |
| `problem` | string | 研究问题 |
| `steps` | `{name, detail}[]` | 方法路径 |
| `findings` | `{value, label}[]` | 关键发现 |
| `limits` | string | 局限与适用边界 |
| `file` | string | 库内 PDF 副本路径 |
| `source` | string | 拖入时的原始路径，用于识别重复拖入 |
| `added` | number | 入库时间（毫秒） |
| `read` `starred` | bool | 已读、星标 |
| `note` | string | 用户笔记 |
| `doi` | string | 规范化后的 DOI（小写、去掉 `https://doi.org/`） |
| `origin` | string | 空 = 拖入解析；`workbuddy` 等 = 外部导入 |
| `rev` | number | 修订时间（毫秒），同步时判新旧用。**不要手写**，`commit_library` 会自动刷 |

所有字段都带 `#[serde(default)]`，旧版本写的 `library.json` 缺字段也能读。模型返回的字段用 `lenient_*` 系列函数宽容解析：`null`、数字、数组形式的作者、`"2025年"` 这类年份都能正确转换。

## Rust 命令

同步相关命令在 `src-tauri/src/sync.rs`，其余在 `lib.rs`。前端用 `invoke("命令名", { 参数 })` 调用（参数名 Rust 用 snake_case，前端用 camelCase，Tauri 自动转换）。

| 命令 | 参数 | 返回 | 作用 |
|---|---|---|---|
| `load_library` | — | `Library` | 读文献库，不存在返回空库 |
| `read_pdf` | `path` | 二进制 | 读 PDF，限 80 MB；用 `ipc::Response` 直接传字节 |
| `parse_pdf` | `path, fileName, pages` | `Paper` | 调模型解析，复制 PDF，入库；同一 `path` 覆盖旧记录并保留用户字段。未配 Key 返回错误 `NO_KEY` |
| `update_paper` | `id, patch` | `Paper` | 改 `tags` / `read` / `starred` / `note`，未给的字段不变 |
| `delete_papers` | `ids` | — | 删除记录和库内 PDF 副本 |
| `rename_tag` | `from, to` | `Library` | 全库重命名主题，`to` 已存在即合并 |
| `delete_tag` | `name` | `Library` | 从所有文献上移除主题 |
| `open_paper` | `id` | — | 用系统默认程序打开 PDF；只接受 id，不接受任意路径 |
| `import_inbox` | — | `ImportResult` | 收取 `inbox/`，见下文 |
| `get_settings` | — | `Settings` | Key 只返回打码版本 |
| `save_settings` | `apiKey, model` | `Settings` | `apiKey` 为 null 表示不修改 |
| `test_connection` | `apiKey` | 模型名列表 | 请求 `/models` 验证 Key；`apiKey` 为 null 时测已保存的 |
| `get_sync_settings` | — | `SyncSettings` | 密码只返回末两位打码 |
| `save_sync_settings` | `url, username, password, insecure, enabled, auto` | `SyncSettings` | `password` 为 null 表示不修改，空串表示清除 |
| `test_sync` | — | string | 建目录并读一次远端库，回报已有多少篇 |
| `sync_now` | — | `SyncReport` | 完整同步；进度通过 `summa://sync` 事件推送 |

**加一个新命令**，固定三步：写 `#[tauri::command] fn xxx(...)` → 加进 `run()` 里的 `generate_handler![...]` → 前端 `invoke("xxx", {...})`。如果用到窗口等系统能力，还要在 `capabilities/default.json` 加权限。

写库一律用 `commit_library(app, &old, &mut new)`，不要直接 `write_library`。前者会逐篇比对并刷新 `rev`，同步靠它判断哪边更新；绕过它，那台机器上的修改就会被另一台的旧数据覆盖。

## 同步

规则与取舍见 [docs/03-sync-spec.md](../docs/03-sync-spec.md)。改这块前请先看第 2 节的状态矩阵——少一个格子就会出现丢数据或删除复活。

算法核心是**三路比对**：本地状态 × 远端状态 × 上次同步快照，一共 5×5 种组合（参考 remotely-save 的 v3 算法）。没有快照时会退化成「并集 + 较新者胜」，所以 `sync-state.json` 不能随便删。

对真实 WebDAV 服务验证 HTTP 语义（默认跳过，需要显式给地址）：

```bash
pip install wsgidav cheroot
python3 -m wsgidav.server.server_cli --host=127.0.0.1 --port=8099 --root=/tmp/davroot --auth=anonymous
cd src-tauri && SUMMA_DAV_TEST_URL=http://127.0.0.1:8099 cargo test webdav
```

之所以要实跑：`MKCOL` 的「已存在」状态码、`HEAD` 是否返回 `ETag`、`If-Match` 冲突是否给 412，各家实现不同，编译期和纯单元测试都看不出来。

## 解析流程

1. 拖入 → `getCurrentWebview().onDragDropEvent()` 拿到路径，进入前端队列（`enqueue` / `runQueue`），逐篇处理
2. `read_pdf` 返回 PDF 字节
3. `renderPdf`（`parse.ts`）用 PDF.js 渲染前 12 页，每页宽 1400px，JPEG 质量 0.82，共用一块画布
4. `parse_pdf` 把页面图像连同 prompt 发给 `https://api.deepseek.com/chat/completions`，模型默认 `deepseek-flash`，`temperature` 0.1，超时 300 秒
5. 从回复中取第一个 `{` 到最后一个 `}` 解析 JSON，宽容转换成 `Paper`
6. 复制 PDF 到 `files/`，写入 `library.json`，返回给界面

参数为什么这样定：

- **1400px 宽**约合 150 DPI，是双栏论文的下限。再低，栏间距会糊连，模型会把左栏末尾和右栏开头读成一句。
- **前 12 页**覆盖绝大多数论文正文，同时控制 token 成本。
- 完整的设计方案（三段式）见 [解析链路技术方案](../docs/01-parsing-spec.md)，当前实现是其简化版。

**prompt 里三条规则不要删**（在 `call_model` 里）：

1. 数值逐字照抄，看不清填 null。模型天生爱做单位换算和四舍五入，一旦发生，用户拿去对比就错了。
2. findings 只取正文用文字强调的结果。图表里有但作者没在文字里主张的数字，是最常见的错误来源。
3. limits 只写作者自己承认的局限。用领域常识补出来的「局限」不是这篇论文的主张。

## 外部导入（收件箱）

完整协议见 [外部工具集成方案](../docs/02-integration-spec.md)，这里只讲应用侧。

- 前端在文献库加载完成后立即调用一次 `import_inbox`，之后每 3 秒一次
- `import_inbox` 读取 `inbox/*.json`（忽略 `.tmp`），每个文件可以是单篇对象、数组或 `{"papers": [...]}`
- `paper_from_external` 兼容两套字段名：skill 写法（`topics`、`method_steps`、`findings` 的 `metric/value/dataset`、`{text, page}` 形式的文本）和 Summa 自己的写法
- 去重：DOI 相同，或 `title_key`（小写、只留字母数字汉字）相同，视为同一篇
- 合并（`merge_into`）：新值非空才覆盖；主题取并集；`read` `starred` `note` `file` 不动
- 成功的文件删除；失败的移到 `inbox/failed/` 并写 `.error.txt`
- 有变化时返回整个库，界面直接替换，并弹出提示

## 主题与样式

- 颜色全部是 `styles.css` 顶部的 CSS 变量。`:root` 是夜间，`:root[data-theme="light"]` 是日间。**新增样式只用变量，不要写死颜色**，否则会在另一套主题下出问题。
- `theme.ts` 把选择存在 `localStorage`（`summa.theme`），把实际主题写到 `<html data-theme>`，并调用 `getCurrentWindow().setTheme()` 让窗口按钮跟着变（需要权限 `core:window:allow-set-theme`）。
- 深色界面用亮度递增表达层级而不是阴影；日间主题下卡片靠细边线区分。强调色只用于选中态、主操作、AI 生成标识。设计令牌见 [品牌与命名基线](../docs/00-brand-naming.md)第 6 节。

## 打包发布

```bash
cd app
npm run tauri build
```

产物：`src-tauri/target/release/bundle/macos/Summa.app`（约 14 MB）。依赖没变时约 1 分钟。

安装到本机（先 `⌘ Q` 退出正在运行的 Summa）：

```bash
rm -rf ~/Applications/Summa.app && cp -R src-tauri/target/release/bundle/macos/Summa.app ~/Applications/
```

发版检查：

1. 版本号三处保持一致：`package.json`、`src-tauri/Cargo.toml`、`src-tauri/tauri.conf.json`
2. `cargo test --lib` 和 `npm run build` 通过
3. 在 [更新日志](../CHANGELOG.md) 记一条
4. 改了数据结构的话，先备份 `library.json` 再装新版验证

打包配置要点：`bundle.targets` 是 `["app"]`（跳过 dmg，避免 AppleScript 权限问题）；`bundle.icon` 要列全 png、`icon.icns`、`icon.ico`。

**图标**：源图 `assets/icon-1024.png` 必须遵循 macOS 图标网格——**1024 画布，图形区 824×824 居中，四周留 100px 透明**，圆角用超椭圆（连续曲率）而不是普通圆角。铺满 1024 的方形图标在 Dock 里会比别的应用大一圈，四角也是直角，一眼就能看出来。改完源图执行：

```bash
npx tauri icon assets/icon-1024.png
```

设计稿如果是一整张带背景的方形图，先用 `assets/compose-icon.py` 处理（抠图 + 套底板 + 排版），它会输出符合网格的源图：

```bash
python3 assets/compose-icon.py 设计稿.png assets/icon-1024.png
```

该脚本依赖 numpy 与 Pillow，只在换图标时需要跑，产物已入库，日常构建不需要执行。

**`bundle.macOS.signingIdentity` 必须是 `"-"`（ad-hoc 签名），不要删。** 这不是为了分发，而是功能必需：macOS 15 起，应用访问局域网（`192.168.x.x`）需要「本地网络」权限，而完全未签名的应用拿不到这个权限，连接会被系统静默切断。表现是同步一直失败、服务端日志里连请求都看不到。删掉这行，局域网 WebDAV 同步就会失效，而指向 `127.0.0.1` 或公网的测试完全正常，很难想到是这个原因。

## 约定与踩过的坑

**窗口拖不动**：两个条件缺一不可，缺了都静默失败。
1. `capabilities/default.json` 必须有 `core:window:allow-start-dragging`，`core:default` 不包含它。
2. `data-tauri-drag-region` 只对元素自身生效，子元素会截断拖拽。标题栏右侧 `.tb-right` 设了 `pointer-events: none`，其中的按钮再单独设回 `auto`；搜索框所在位置不能拖窗口。

**PDF.js v6**：`getDocument` 要传 `{ data }` 不能传路径；`render` 必须传 `canvas`，同时传 `canvasContext` 会冲突；销毁用 `loadingTask.destroy()`。

**中文字符串截断**：Rust 里不要写 `&s[..n]`，切在汉字中间会崩溃。用 `clip(s, n)`。

**模型输出**：不要假设字段类型一定对。新增字段时也用 `lenient_*` 反序列化。

**弹窗**：不要用 `alert()` / `confirm()`，WebView 里行为不可靠。确认操作用「点两次」，错误用错误框（`showError`）。

**环境变量**：macOS 双击启动的 .app 不继承终端环境变量，所以打包版只能从 `config.json` 读 Key；`DEEPSEEK_API_KEY` 只在 `npm start` 时有用。

**`target/` 占好几 GB**：编译缓存，别删，删了要重新编译约 10 分钟。

**界面用词**：遵循品牌基线第 3 节——要义页（不说摘要、卡片）、主题（不说标签、文件夹、分类）、研究问题、方法路径；用户手动拖入叫投递。外部工具写入库里的动作是另一回事，称「导入」。文案保持克制，不用感叹号，不用「颠覆」「革命」之类的词。

## 下一步

1. 数字准确率评测：20 篇论文对照原文，目标 ≥ 97%（方法见解析方案第 10 节）
2. 文献库迁移到 SQLite（几千篇后 `library.json` 整体读写会变慢）
3. 主题总览页：主题定义、年度分布
4. 语义检索：现在只是关键词匹配
5. Windows 打包与验证
