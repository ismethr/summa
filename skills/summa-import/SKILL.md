---
name: summa-import
description: 把论文阅读结果导入用户本地的 Summa 文献管理软件，或检索用户 Summa 库里已有的文献。当用户说「存到 Summa」「导入文献库」「记下这篇论文」「加进我的库」「归档一下」，或你分析完一篇论文后用户想保存时使用；用户在写作、综述、对比任务中提到「我库里的」「我之前读过的」时，也用它检索。
allowed-tools: "Read Bash"
metadata:
  version: "2.0"
---

# Summa 文献库导入与检索

Summa 是用户电脑上的文献管理桌面应用。你通过本技能自带的脚本读写它的文献库：

```bash
SUMMA="python3 ~/.workbuddy/skills/summa-import/scripts/summa.py"
```

（如果本技能装在别处，把路径换成本技能目录下的 `scripts/summa.py`。）

脚本只用 Python 标准库，所有命令都输出 JSON，`"ok": false` 时看 `error` 和 `hint`。

## 导入一篇论文

### 1. 先看已有主题

```bash
$SUMMA topics
```

给论文定主题时**优先复用库里已有的主题名**，不要造近义词（库里有「变化检测」就不要再写「SAR 变化检测方法」）。确实是新方向才新建。每篇 1–3 个主题。

### 2. 写好 JSON，提交

把内容写进一个临时文件（如 `/tmp/summa-paper.json`），然后：

```bash
$SUMMA add /tmp/summa-paper.json
```

如果论文是用户电脑上的 PDF 文件，加上 `--pdf`，用户就能在 Summa 里直接打开原文：

```bash
$SUMMA add /tmp/summa-paper.json --pdf "/Users/xxx/Downloads/paper.pdf"
```

JSON 格式（只有 `title` 必填，其余能提取多少填多少，**拿不准的留空或填 null，不要编造**）：

```json
{
  "title": "论文原文标题",
  "doi": "10.1109/TGRS.2025.3412345",
  "authors": ["Chen, L.", "Wang, Y."],
  "venue": "IEEE TGRS",
  "year": 2025,
  "topics": ["基础模型", "多模态融合"],
  "gist": "一句话要旨，不超过 60 字：这篇做了什么、结果如何",
  "problem": "作者要解决什么问题",
  "method_steps": [
    { "name": "预训练语料", "detail": "做了什么" }
  ],
  "findings": [
    { "metric": "mIoU", "value": "+12.4%", "dataset": "LoveDA" }
  ],
  "limits": "作者自述的局限",
  "source": "workbuddy"
}
```

| 字段 | 要求 |
|---|---|
| `title` | 必填。用原文标题，不翻译 |
| `doi` | 有就填，不要从标题推测 |
| `authors` | 数组，格式 `"姓, 名首字母."` |
| `topics` | 1–3 个，优先复用 `topics` 命令列出的已有主题 |
| `gist` | 一句话，Summa 列表和要义页的核心 |
| `method_steps` | 2–4 步，`name` 是短的动宾短语 |
| `findings` | 最多 4 条，`metric + value + dataset` 三元组 |
| `limits` | 只写作者自己承认的局限 |
| `source` | 填你所在的工具名：`workbuddy` / `claude-code` / `codex` |

一次导入多篇时，用 `{"papers": [ {...}, {...} ]}`，但每篇都要单独认真抽取。

### 3. 如实汇报结果

- `"status": "imported"` → 告诉用户「已导入 Summa」
- `"status": "queued"` → 告诉用户「Summa 现在没开，已放进收件箱，下次打开 Summa 会自动导入」
- `"ok": false` → 把 `error` 原样转告；如果是格式问题，修正后重新提交一次

同一篇论文（DOI 或标题相同）重复导入不会产生两条：Summa 会合并，新内容非空的字段覆盖旧的，主题取并集，用户自己的笔记、星标、已读状态不受影响。

## 三条硬性质量规则

1. **数值逐字照抄**，不换算单位、不改格式、不四舍五入。看不清就填 `null`。
2. **`findings` 只取正文用文字强调过的结果**。图表里有但作者没在正文主张的数字不要写——这是最常见的错误来源。
3. **`limits` 只取自作者的讨论或局限章节**。你推断出的局限不是这篇论文的主张，写进去在学术上不成立。

## 检索用户已有的文献

用户在写作、综述、对比时提到「我库里的」「我读过的」，先查：

```bash
$SUMMA search SAR 变化检测
$SUMMA search 扩散模型 --limit 20
```

多个词之间是「且」的关系，匹配题名、作者、期刊、主题、要旨、方法、用户笔记。需要细节再取单篇：

```bash
$SUMMA get p1730000000000
```

库里没有相关文献时，如实说「你的 Summa 库里没有找到」，**不要用通用知识冒充用户的收藏**。

## 其他

```bash
$SUMMA status   # 数据目录、篇数、收件箱里待导入和格式有误的文件
```

`summa_installed: false` 表示用户还没装或从没打开过 Summa，请用户先打开一次。

## 不要做的事

- 不要直接修改 `library.json`，只能通过 `add` 提交——Summa 正开着时直接改会被覆盖或写坏
- 不要为凑字段编造 DOI、作者、数值
- 抓取或读取论文失败时不要导入残缺记录，宁可这次不导
