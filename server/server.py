#!/usr/bin/env python3
"""
已停用：外部工具导入改为 Summa 应用的收件箱机制，见 skills/summa-import。
本服务写的是另一个独立数据库，Summa 应用看不到其中的数据。

Summa 本地接口服务 —— 零依赖，Python 3.9+ 可直接运行。

用途：让外部 AI 工具（Claude Code / Codex / WorkBuddy 等）
读取与写入 Summa 文献库，而不必直接操作数据库文件。

启动：
    python3 server.py
    SUMMA_PORT=7823 SUMMA_DB=~/.summa/library.db python3 server.py

接口：
    GET  /v1/health              健康检查
    GET  /v1/stats               库统计
    GET  /v1/papers?q=&limit=    检索（q 为空返回最近）
    GET  /v1/papers/{id}         读取完整要义页
    POST /v1/papers              导入（按 DOI / 标题去重，幂等）

设计说明：
    · 外部工具必须走 HTTP 而不是直连 SQLite——写入需要通过服务
      触发索引更新与主题重算，绕过服务会留下不一致状态。
    · 去重键优先 DOI，其次规范化标题（小写、去标点、去空白）。
    · 重复导入不报错，返回 action=updated 并覆盖 payload，
      但保留 source 字段，以便区分「谁写的这份摘要」。
"""

import json
import os
import re
import sqlite3
import unicodedata
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import urlparse, parse_qs

DB_PATH = os.environ.get("SUMMA_DB", os.path.expanduser("~/.summa/library.db"))
PORT = int(os.environ.get("SUMMA_PORT", "7823"))
VERSION = "0.1.0"

SCHEMA = """
CREATE TABLE IF NOT EXISTS papers (
    id          TEXT PRIMARY KEY,
    doi         TEXT,
    title       TEXT NOT NULL,
    title_key   TEXT NOT NULL,
    authors     TEXT,
    venue       TEXT,
    year        INTEGER,
    source      TEXT,
    payload     TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_title_key ON papers(title_key);
CREATE INDEX IF NOT EXISTS idx_doi ON papers(doi);
"""


# ---------- 工具函数 ----------

def now():
    return datetime.now(timezone.utc).isoformat(timespec="seconds")


def norm_doi(doi):
    if not doi:
        return None
    d = str(doi).strip().lower()
    d = re.sub(r"^https?://(dx\.)?doi\.org/", "", d)
    d = re.sub(r"^doi:\s*", "", d)
    return d or None


def norm_title(title):
    t = unicodedata.normalize("NFKC", title or "").lower()
    t = re.sub(r"[^0-9a-z\u4e00-\u9fff]+", "", t)
    return t


def make_id(title_key):
    import hashlib
    return hashlib.sha1(title_key.encode("utf-8")).hexdigest()[:12]


def connect():
    d = os.path.dirname(os.path.abspath(DB_PATH))
    if d:
        os.makedirs(d, exist_ok=True)
    con = sqlite3.connect(DB_PATH)
    con.row_factory = sqlite3.Row
    con.executescript(SCHEMA)
    return con


def flatten(payload):
    """兼容两种入参：扁平结构，或 {meta:{...}, ...} 嵌套结构。"""
    meta = payload.get("meta") or {}
    authors = payload.get("authors") or meta.get("authors") or []
    if isinstance(authors, list):
        authors = ", ".join(str(a) for a in authors if a)
    return {
        "title": payload.get("title") or meta.get("title") or "",
        "doi": payload.get("doi") or meta.get("doi"),
        "authors": authors,
        "venue": payload.get("venue") or meta.get("venue") or "",
        "year": payload.get("year") or meta.get("year"),
        "gist": payload.get("gist") or "",
    }


def snippet(row, n=140):
    try:
        p = json.loads(row["payload"])
    except Exception:
        p = {}
    gist = p.get("gist") or ""
    if len(gist) > n:
        gist = gist[:n] + "…"
    topics = [t.get("name") for t in (p.get("topics") or []) if isinstance(t, dict)]
    return {
        "id": row["id"],
        "title": row["title"],
        "authors": row["authors"],
        "venue": row["venue"],
        "year": row["year"],
        "source": row["source"],
        "topics": topics,
        "gist": gist,
        "updated_at": row["updated_at"],
    }


# ---------- 业务 ----------

def merge_payload(old, new):
    """字段级合并：空值不覆盖已有内容。

    动机：外部工具导入的 payload 往往只有部分字段（比如只有标题和一句话摘要），
    若整体覆盖，会把此前的完整要义页冲成残缺版本。这里只让非空字段生效。
    需要强制覆盖时，调用方传 "_mode": "replace"。
    """
    if not isinstance(old, dict):
        return new
    out = dict(old)
    for k, v in new.items():
        if v is None or v == "" or v == [] or v == {}:
            continue
        if isinstance(v, dict) and isinstance(out.get(k), dict):
            out[k] = merge_payload(out[k], v)
        else:
            out[k] = v
    return out


def ingest(payload):
    f = flatten(payload)
    if not f["title"].strip():
        return 400, {"error": "missing_title", "hint": "标题是唯一必填字段"}

    tk = norm_title(f["title"])
    pid = make_id(tk)
    doi = norm_doi(f["doi"])
    ts = now()
    clean = {k: v for k, v in payload.items() if not k.startswith("_")}

    con = connect()
    try:
        row = None
        if doi:
            row = con.execute("SELECT * FROM papers WHERE doi = ?", (doi,)).fetchone()
        if row is None:
            row = con.execute("SELECT * FROM papers WHERE title_key = ?", (tk,)).fetchone()

        if row:
            try:
                old_payload = json.loads(row["payload"])
            except Exception:
                old_payload = {}
            if payload.get("_mode") == "replace":
                merged = clean
            else:
                merged = merge_payload(old_payload, clean)
            con.execute(
                """UPDATE papers SET doi=?, title=?, authors=?, venue=?, year=?,
                   source=?, payload=?, updated_at=? WHERE id=?""",
                (doi or row["doi"], f["title"], f["authors"], f["venue"],
                 f["year"], payload.get("source") or row["source"],
                 json.dumps(merged, ensure_ascii=False), ts, row["id"]),
            )
            con.commit()
            return 200, {"id": row["id"], "action": "updated",
                         "message": "已存在同篇文献，已按字段合并"}
        else:
            con.execute(
                """INSERT INTO papers
                   (id, doi, title, title_key, authors, venue, year, source, payload, created_at, updated_at)
                   VALUES (?,?,?,?,?,?,?,?,?,?,?)""",
                (pid, doi, f["title"], tk, f["authors"], f["venue"], f["year"],
                 payload.get("source") or "external",
                 json.dumps(clean, ensure_ascii=False), ts, ts),
            )
            con.commit()
            return 201, {"id": pid, "action": "created", "message": "已导入"}
    finally:
        con.close()


def search(q, limit=20):
    con = connect()
    try:
        limit = max(1, min(100, limit))
        if q.strip():
            like = f"%{q.strip()}%"
            rows = con.execute(
                """SELECT * FROM papers
                   WHERE title LIKE ? OR authors LIKE ? OR venue LIKE ? OR payload LIKE ?
                   ORDER BY updated_at DESC LIMIT ?""",
                (like, like, like, like, limit),
            ).fetchall()
        else:
            rows = con.execute(
                "SELECT * FROM papers ORDER BY updated_at DESC LIMIT ?", (limit,)
            ).fetchall()
        return {"count": len(rows), "items": [snippet(r) for r in rows]}
    finally:
        con.close()


def get_paper(pid):
    con = connect()
    try:
        row = con.execute("SELECT * FROM papers WHERE id = ?", (pid,)).fetchone()
        if not row:
            return None
        try:
            payload = json.loads(row["payload"])
        except Exception:
            payload = {}
        return {
            "id": row["id"],
            "title": row["title"],
            "doi": row["doi"],
            "authors": row["authors"],
            "venue": row["venue"],
            "year": row["year"],
            "source": row["source"],
            "created_at": row["created_at"],
            "updated_at": row["updated_at"],
            "record": payload,
        }
    finally:
        con.close()


def stats():
    con = connect()
    try:
        total = con.execute("SELECT COUNT(*) c FROM papers").fetchone()["c"]
        by_source = {
            r["source"] or "unknown": r["c"]
            for r in con.execute(
                "SELECT source, COUNT(*) c FROM papers GROUP BY source"
            ).fetchall()
        }
        last = con.execute("SELECT MAX(updated_at) t FROM papers").fetchone()["t"]
        return {"total": total, "by_source": by_source, "last_updated": last}
    finally:
        con.close()


# ---------- HTTP ----------

class Handler(BaseHTTPRequestHandler):
    server_version = "SummaLocal/" + VERSION

    def log_message(self, fmt, *args):
        print("[summa] " + (fmt % args), flush=True)

    def _send(self, code, obj):
        body = json.dumps(obj, ensure_ascii=False).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Access-Control-Allow-Headers", "Content-Type")
        self.send_header("Access-Control-Allow-Methods", "GET,POST,OPTIONS")
        self.end_headers()
        self.wfile.write(body)

    def do_OPTIONS(self):
        self._send(204, {})

    def do_GET(self):
        u = urlparse(self.path)
        path = u.path.rstrip("/") or "/"
        qs = parse_qs(u.query)

        if path == "/v1/health":
            return self._send(200, {"ok": True, "version": VERSION, "db": DB_PATH})
        if path == "/v1/stats":
            return self._send(200, stats())
        if path == "/v1/papers":
            q = qs.get("q", [""])[0]
            try:
                limit = int(qs.get("limit", ["20"])[0])
            except ValueError:
                limit = 20
            return self._send(200, search(q, limit))
        if path.startswith("/v1/papers/"):
            pid = path.rsplit("/", 1)[-1]
            obj = get_paper(pid)
            if obj is None:
                return self._send(404, {"error": "not_found", "id": pid})
            return self._send(200, obj)

        self._send(404, {"error": "unknown_endpoint", "path": path})

    def do_POST(self):
        u = urlparse(self.path)
        path = u.path.rstrip("/") or "/"
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length) if length else b""

        if path != "/v1/papers":
            return self._send(404, {"error": "unknown_endpoint", "path": path})

        try:
            payload = json.loads(raw.decode("utf-8"))
        except Exception as e:
            return self._send(400, {"error": "invalid_json", "detail": str(e)})
        if not isinstance(payload, dict):
            return self._send(400, {"error": "invalid_payload",
                                    "hint": "body 必须是 JSON 对象"})

        code, obj = ingest(payload)
        self._send(code, obj)


def main():
    con = connect()
    con.close()
    print(f"Summa local API  http://127.0.0.1:{PORT}")
    print(f"database         {DB_PATH}")
    print("endpoints        GET /v1/health · /v1/stats · /v1/papers?q= · /v1/papers/{id}")
    print("                 POST /v1/papers")
    print("Ctrl+C to stop.\n", flush=True)
    HTTPServer(("127.0.0.1", PORT), Handler).serve_forever()


if __name__ == "__main__":
    main()
