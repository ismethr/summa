# Summa 外部工具集成方案

> 版本：v2.0 ｜ 2026-10-28
> 目标：让 WorkBuddy、Claude Code、Codex 等 AI 工具能够把论文写进 Summa 文献库，并检索用户已有的文献。
> v1.0（本地 HTTP 服务 `server/server.py`）已停用，原因见第 8 节。

---

## 0. 核心判断

**Skill 只是说明书，能力要来自 Summa 本身。**

Skill 告诉 AI 工具「何时调用、调用什么、按什么质量标准填字段」，它自己没有任何能力。能力必须由 Summa 提供，并且写进的必须是 Summa 应用实际在用的那个文献库——否则就是导进了一个用户看不到的地方。

```
Skill（SKILL.md：何时用、怎么填、质量规则）
   ↓ 调用
summa.py（命令行脚本，只用 Python 标准库）
   ↓ 写：放文件到 inbox/          ↓ 读：只读 library.json
Summa 应用（唯一写入 library.json 的进程）
```

---

## 1. 为什么是收件箱

外部工具和 Summa 应用是两个独立进程，都想改同一个文献库。可选的做法有三种：

| 方式 | 问题 |
|---|---|
| 外部工具直接改 `library.json` | 应用开着时，界面下一次保存会把外部写入覆盖掉；两边同时写会写坏文件 |
| 应用内嵌 HTTP 服务 | 应用没开时无法写入；要占端口、处理鉴权 |
| **收件箱：外部工具放文件，应用来收** | 应用没开时先排队，打开后自动导入；`library.json` 只有一个写入者 |

收件箱的约束只有一条，且不可妥协：**外部工具不得直接修改 `library.json`**。所有写入都是往 `inbox/` 放文件，由应用在持有全局锁的情况下合并进库。

读取不受这条约束：`summa.py search/get/topics` 直接只读 `library.json`。读到的可能比界面晚几秒，对检索场景无影响。

---

## 2. 目录与文件

数据目录：

| 平台 | 路径 |
|---|---|
| macOS | `~/Library/Application Support/app.summa.desktop/` |
| Windows | `%APPDATA%\app.summa.desktop\` |
| Linux | `~/.local/share/app.summa.desktop/` |

可用环境变量 `SUMMA_DATA_DIR` 覆盖（仅 `summa.py` 读取，用于测试）。

```
app.summa.desktop/
├── library.json        文献库（只有应用写）
├── files/              PDF 副本
├── inbox/              ← 外部工具往这里放 *.json
│   └── failed/         格式有误的文件 + 同名 .error.txt
└── config.json
```

---

## 3. 写入协议

### 3.1 提交

1. 写临时文件 `inbox/<任意名>.json.tmp`
2. 写完后改名为 `inbox/<任意名>.json`

改名是原子操作，应用永远不会读到写了一半的文件（应用只认 `.json` 结尾）。`summa.py add` 已经这样做，文件名形如 `1793116837475-论文标题前缀.json`。

### 3.2 应用收取

- 时机：文献库加载完成后立即一次，之后每 3 秒一次；应用没开时文件留在收件箱
- 按文件名排序逐个处理
- 每个文件可以是单篇对象、数组，或 `{"papers": [...]}`
- 整个文件都成功：删除文件
- 有任何一篇失败：文件移到 `inbox/failed/`，旁边写 `<文件名>.error.txt`；同一文件里成功的那几篇仍会入库
- 有新增或更新时，界面刷新并在底部提示「已从外部工具导入：新增 N 篇，更新 M 篇」

### 3.3 确认结果

`summa.py add` 提交后最多等 8 秒：

| 情况 | 输出 `status` | 含义 |
|---|---|---|
| 文件被收走 | `imported` | 应用开着，已入库 |
| 文件出现在 `failed/` | `ok: false` + 原因 | 被拒收 |
| 8 秒后还在 | `queued` | 应用没开，下次启动导入 |

---

## 4. 数据契约

只有 `title` 必填。字段名兼容两套写法：skill 推荐的写法，和 Summa 内部 `Paper` 的写法。

| 字段 | 接受的形式 | 存为 |
|---|---|---|
| `title` | 字符串 | `title` |
| `authors` | 数组或字符串 | 逗号分隔字符串 |
| `venue` | 字符串 | `venue` |
| `year` | 数字或含年份的字符串（`"2025年"`） | 数字，未知为 0 |
| `doi` | 字符串，可带 `https://doi.org/` 或 `doi:` 前缀 | 规范化：小写、去前缀 |
| `topics` 或 `tags` | 字符串数组、`{name}` 对象数组、或逗号分隔字符串 | `tags` |
| `gist` | 字符串 | `gist` |
| `problem` | 字符串或 `{text, page}` | 文本 |
| `method_steps` 或 `steps` | `[{name, detail, page?}]` | `steps` |
| `findings` | `[{metric, value, dataset, page?}]` 或 `[{value, label}]` | `value` + `label`（`label` = `metric · dataset`） |
| `limits` | 字符串或 `{text, page}` | 文本 |
| `source` | 工具名 | `origin`，界面显示「来自 xxx」；缺省为「外部导入」 |
| `pdf_path` | 本机 PDF 的绝对路径 | 复制到 `files/`，可在界面打开 |

说明：

- `page` 字段目前会被接受但不保存。界面还没有「跳到原文第几页」的能力，等那时再存。
- 未列出的字段（如 `confidence`）被忽略，不会报错。
- 没有主题的新文献会被归入「待归类」。

最小可用 payload：

```json
{ "title": "论文标题" }
```

---

## 5. 去重与合并

### 去重

满足任一条件即视为同一篇：

1. DOI 都不为空且规范化后相同
2. 标题规范化后相同：转小写，只保留字母、数字和汉字

### 合并

重复导入不整体覆盖，按字段合并：

| 字段 | 规则 |
|---|---|
| 文本字段（标题、作者、期刊、要旨、问题、局限、DOI） | 新值非空才覆盖 |
| `year` | 新值非 0 才覆盖 |
| `steps` `findings` | 新数组非空才整体替换 |
| `tags` | 取并集，保持原顺序 |
| `read` `starred` `note` | 永不改动——这些是用户自己的 |
| `file` | 已有 PDF 副本时不动；没有且提交了 `pdf_path` 时补上 |

这条规则来自实测：外部工具常常只提交标题和一句话要旨，如果整体覆盖，会把应用解析出的完整要义页冲成残缺版本。

注意：外部导入和拖入解析共用同一个库。拖入的论文如果标题相同，外部导入会合并进那条记录。

---

## 6. 读取

| 命令 | 返回 |
|---|---|
| `summa.py status` | 数据目录、篇数、收件箱待导入和失败的文件 |
| `summa.py topics` | 已有主题及篇数，按篇数降序 |
| `summa.py search 词1 词2 [--limit N]` | 精简列表：id、标题、作者、期刊、年份、主题、要旨、已读、星标。默认最多 10 条 |
| `summa.py get <id>` | 完整记录（不含本机文件路径） |

检索匹配题名、作者、期刊、年份、要旨、问题、局限、方法步骤、主题和用户笔记，多个词之间是「且」。

**双向才有价值**。单向导入解决的是「存进去」；真正的价值在反方向：用户写论文时问「我库里关于 SAR 变化检测的无监督方法有哪些」，拿到的是他自己读过、可能已经忘了的文献。通用 AI 助手给不了这个能力，因为它不知道用户读过什么。**Summa 的护城河不是解析能力，是用户自己的文献积累。** 所以检索和写入同期提供。

---

## 7. 质量规则

这三条写在 SKILL.md 里，与应用内解析的 prompt 保持一致：

1. **数值逐字照抄**，不换算单位、不改格式、不四舍五入；看不清填 `null`
2. **`findings` 只取正文用文字强调过的结果**，不收图表里出现但作者没在正文主张的数字
3. **`limits` 只取作者自己承认的局限**，不用领域知识补充

另外，skill 要求 AI 工具在导入前先用 `topics` 查看已有主题并优先复用，避免近义主题泛滥。

---

## 8. 为什么停用 v1（HTTP 服务）

v1 是一个独立的 Python HTTP 服务（`server/server.py`，端口 7823），数据存在 `~/.summa/library.db`（SQLite）。它本身工作正常，但：

- 桌面应用读写的是 `library.json`，**根本不读这个 SQLite**——导进去的论文在 Summa 里看不到
- 需要用户手动启动服务，应用没开时也无法排队
- 要维护两套数据结构

v2 让应用自己成为唯一写入者，这些问题同时消失。`server/` 目录保留作参考，不再使用。

---

## 9. 安装与验证

安装 skill：

```bash
# WorkBuddy
cp -R skills/summa-import ~/.workbuddy/skills/

# Claude Code
cp -R skills/summa-import ~/.claude/skills/
```

手动验证：

```bash
S=skills/summa-import/scripts/summa.py
python3 $S status
echo '{"title":"集成测试（可删除）","gist":"验证用","source":"manual"}' | python3 $S add -
python3 $S search 集成测试
```

Summa 开着时第二条应返回 `"status": "imported"`，并在界面看到这条记录；没开时返回 `queued`，打开 Summa 后自动出现。

---

## 10. 已知限制

- **检索是关键词匹配**，不是语义检索。原文没出现查询词就会漏
- **收件箱轮询间隔 3 秒**，应用开着时导入有最多几秒延迟
- **`page` 未保存**，用户暂时不能从要义页跳回原文对应页
- **`origin` 只保留第一次导入的来源**，多个工具贡献同一篇时无法追溯
- **本机任意进程都能往收件箱放文件**。当前可接受；若将来支持多机共享文献库，需要重新设计
- **MCP Server 未做**。Skill + 脚本覆盖了所有有命令行能力的 AI 工具；MCP 的好处是工具可被自动发现，留作后续
