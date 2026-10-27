#!/usr/bin/env python3
"""
Summa 文献库命令行工具 —— 给 WorkBuddy / Claude Code 等 AI 工具用。
只依赖 Python 标准库。

    python3 summa.py status                  文献库位置、篇数、收件箱待导入数
    python3 summa.py topics                  列出已有主题（导入前先看，尽量复用）
    python3 summa.py search "SAR 变化检测"     检索文献库
    python3 summa.py get <id>                读取一篇的完整内容
    python3 summa.py add paper.json          导入（文件可含单篇、数组或 {"papers": [...]}）
    python3 summa.py add - < paper.json      从标准输入导入
    python3 summa.py add paper.json --pdf /path/to/paper.pdf   同时附上 PDF

写入方式：把 JSON 放进 Summa 数据目录下的 inbox/，由 Summa 应用自己收进文献库。
应用开着时几秒内导入；没开时下次启动导入。本工具从不直接改 library.json。
"""

import json
import os
import re
import sys
import time
from pathlib import Path

APP_ID = "app.summa.desktop"


def data_dir() -> Path:
    if os.environ.get("SUMMA_DATA_DIR"):
        return Path(os.environ["SUMMA_DATA_DIR"]).expanduser()
    if sys.platform == "darwin":
        return Path.home() / "Library" / "Application Support" / APP_ID
    if os.name == "nt":
        return Path(os.environ.get("APPDATA", Path.home())) / APP_ID
    return Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local" / "share")) / APP_ID


def out(obj) -> None:
    print(json.dumps(obj, ensure_ascii=False, indent=2))


def fail(msg: str, hint: str = "") -> None:
    out({"ok": False, "error": msg, **({"hint": hint} if hint else {})})
    sys.exit(1)


def load_library() -> list:
    lib = data_dir() / "library.json"
    if not lib.exists():
        return []
    try:
        return json.loads(lib.read_text("utf-8")).get("papers", [])
    except Exception as e:
        fail(f"文献库读取失败：{e}")
        return []


def inbox_pending() -> list:
    inbox = data_dir() / "inbox"
    return sorted(p.name for p in inbox.glob("*.json")) if inbox.exists() else []


# ---------- 读取 ----------

def cmd_status(_args) -> None:
    d = data_dir()
    papers = load_library()
    failed = d / "inbox" / "failed"
    out({
        "ok": True,
        "data_dir": str(d),
        "summa_installed": d.exists(),
        "papers": len(papers),
        "inbox_pending": inbox_pending(),
        "inbox_failed": sorted(p.name for p in failed.glob("*.json")) if failed.exists() else [],
    })


def cmd_topics(_args) -> None:
    counts: dict = {}
    for p in load_library():
        for t in p.get("tags", []):
            counts[t] = counts.get(t, 0) + 1
    out({"ok": True, "topics": [{"name": k, "count": v} for k, v in sorted(counts.items(), key=lambda x: -x[1])]})


def brief(p: dict) -> dict:
    return {
        "id": p.get("id"),
        "title": p.get("title"),
        "authors": p.get("authors"),
        "venue": p.get("venue"),
        "year": p.get("year") or None,
        "topics": p.get("tags", []),
        "gist": p.get("gist"),
        "read": p.get("read", False),
        "starred": p.get("starred", False),
    }


def cmd_search(args) -> None:
    limit = 10
    if "--limit" in args:
        i = args.index("--limit")
        limit = int(args[i + 1])
        args = args[:i] + args[i + 2:]
    words = " ".join(args).lower().split()
    hits = []
    for p in load_library():
        hay = " ".join([
            p.get("title", ""), p.get("authors", ""), p.get("venue", ""), str(p.get("year", "")),
            p.get("gist", ""), p.get("problem", ""), p.get("limits", ""), p.get("note", ""),
            " ".join(p.get("tags", [])), " ".join(s.get("name", "") + s.get("detail", "") for s in p.get("steps", [])),
        ]).lower()
        if all(w in hay for w in words):
            hits.append(brief(p))
    out({"ok": True, "query": " ".join(words), "total": len(hits), "papers": hits[:limit]})


def cmd_get(args) -> None:
    if not args:
        fail("用法：summa.py get <id>")
    for p in load_library():
        if p.get("id") == args[0]:
            p = dict(p)
            for k in ("file", "source"):
                p.pop(k, None)
            out({"ok": True, "paper": p})
            return
    fail(f"找不到 id 为 {args[0]} 的文献", "先用 search 查到 id")


# ---------- 写入 ----------

def check(paper: dict, i: int) -> list:
    """返回问题列表；有问题就不写入"""
    where = f"第 {i + 1} 篇" if i >= 0 else "这篇"
    problems = []
    if not isinstance(paper, dict):
        return [f"{where}不是 JSON 对象"]
    if not str(paper.get("title") or "").strip():
        problems.append(f"{where}缺少 title")
    if paper.get("authors") is not None and not isinstance(paper["authors"], (list, str)):
        problems.append(f"{where}的 authors 应是数组或字符串")
    for f in paper.get("findings") or []:
        if not isinstance(f, dict):
            problems.append(f"{where}的 findings 每条应是对象")
            break
    return problems


def cmd_add(args) -> None:
    pdf = ""
    if "--pdf" in args:
        i = args.index("--pdf")
        pdf = str(Path(args[i + 1]).expanduser().resolve())
        args = args[:i] + args[i + 2:]
        if not Path(pdf).is_file():
            fail(f"PDF 不存在：{pdf}")
    if not args:
        fail("用法：summa.py add <paper.json | ->")
    raw = sys.stdin.read() if args[0] == "-" else Path(args[0]).expanduser().read_text("utf-8")
    try:
        data = json.loads(raw)
    except json.JSONDecodeError as e:
        fail(f"不是合法 JSON：{e}")

    papers = data if isinstance(data, list) else data.get("papers", [data]) if isinstance(data, dict) else []
    if not papers:
        fail("没有找到论文数据")
    problems = [m for i, p in enumerate(papers) for m in check(p, i if len(papers) > 1 else -1)]
    if problems:
        fail("；".join(problems), "修正后重新提交，不要写入残缺记录")
    if pdf:
        if len(papers) != 1:
            fail("--pdf 只能用于单篇导入")
        papers[0]["pdf_path"] = pdf
    for p in papers:
        p.setdefault("source", "workbuddy")

    d = data_dir()
    if not d.exists():
        fail("没有找到 Summa 数据目录", "请先打开一次 Summa 应用")
    inbox = d / "inbox"
    inbox.mkdir(parents=True, exist_ok=True)
    slug = re.sub(r"[^0-9A-Za-z一-鿿]+", "-", str(papers[0]["title"]))[:40].strip("-") or "paper"
    name = f"{int(time.time() * 1000)}-{slug}.json"
    tmp = inbox / (name + ".tmp")
    # 先写临时文件再改名，Summa 不会读到写了一半的文件
    tmp.write_text(json.dumps({"papers": papers}, ensure_ascii=False, indent=2), "utf-8")
    tmp.rename(inbox / name)

    # Summa 开着时每 3 秒收一次件；等一会儿看是否已被收走
    for _ in range(16):
        time.sleep(0.5)
        if not (inbox / name).exists():
            failed = inbox / "failed" / name
            if failed.exists():
                err = (inbox / "failed" / f"{name}.error.txt")
                fail("Summa 拒收了这份数据：" + (err.read_text("utf-8") if err.exists() else "原因未知"))
            out({"ok": True, "status": "imported", "titles": [p["title"] for p in papers],
                 "message": "已导入 Summa，界面已刷新"})
            return
    out({"ok": True, "status": "queued", "titles": [p["title"] for p in papers],
         "message": "Summa 现在没有打开，已放入收件箱，下次打开 Summa 时会自动导入"})


COMMANDS = {"status": cmd_status, "topics": cmd_topics, "search": cmd_search, "get": cmd_get, "add": cmd_add}

if __name__ == "__main__":
    if len(sys.argv) < 2 or sys.argv[1] not in COMMANDS:
        print(__doc__)
        sys.exit(1)
    COMMANDS[sys.argv[1]](sys.argv[2:])
