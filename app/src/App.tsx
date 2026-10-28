import { type MouseEvent, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import type { ImportResult, Library, Paper, Settings, SyncReport, SyncSettings } from "./types";
import { renderPdf } from "./parse";
import SettingsDialog from "./Settings";
import Detail, { type PaperChanges } from "./Detail";
import BulkPanel from "./BulkPanel";
import { useTheme } from "./theme";
import sample from "./sample.json";

const COLORS = ["#C4795F", "#7FB08A", "#7C9CC4", "#C4A85E", "#A48FB4", "#6E6E7A"];
const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

type Job = { name: string; stage: string; pct: number; error: string; queued: number };
type View = "all" | "unread" | "starred" | "recent";
type Sort = "added" | "year" | "title";

const VIEWS: { id: View; label: string }[] = [
  { id: "all", label: "全部文献" },
  { id: "unread", label: "待读" },
  { id: "starred", label: "星标" },
  { id: "recent", label: "最近 7 天" },
];

const WEEK = 7 * 24 * 3600 * 1000;

function inView(p: Paper, view: View) {
  if (view === "unread") return !p.read;
  if (view === "starred") return !!p.starred;
  if (view === "recent") return (p.added ?? 0) > Date.now() - WEEK;
  return true;
}

function lastSyncText(ts: number) {
  if (!ts) return "未同步";
  const d = Date.now() - ts;
  if (d < 60_000) return "刚刚";
  if (d < 3600_000) return `${Math.floor(d / 60_000)} 分钟前`;
  if (d < 86400_000) return `${Math.floor(d / 3600_000)} 小时前`;
  return `${Math.floor(d / 86400_000)} 天前`;
}

/** 搜索时比对的文本：题名、作者、期刊、主题、要旨、方法、笔记 */
function haystack(p: Paper) {
  return [p.title, p.authors, p.venue, String(p.year || ""), p.gist, p.note ?? "", ...p.tags, ...p.steps.map((s) => s.name)]
    .join(" ")
    .toLowerCase();
}

export default function App() {
  const [papers, setPapers] = useState<Paper[]>([]);
  const [activeId, setActiveId] = useState("");
  const [status, setStatus] = useState<"loading" | "ready" | "error">("loading");
  const [error, setError] = useState("");
  const [job, setJob] = useState<Job | null>(null);
  const [dragging, setDragging] = useState(false);
  const [query, setQuery] = useState("");
  const [topic, setTopic] = useState("");
  const [view, setView] = useState<View>("all");
  const [sort, setSort] = useState<Sort>("added");
  const [renaming, setRenaming] = useState<{ from: string; to: string } | null>(null);
  const [deletingTag, setDeletingTag] = useState("");
  // 多选：⌘ 点击逐个加选，Shift 点击连续选择
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [toast, setToast] = useState("");
  const [settings, setSettings] = useState<Settings | null>(null);
  const [sync, setSync] = useState<SyncSettings | null>(null);
  const [syncing, setSyncing] = useState(false);
  const [syncStage, setSyncStage] = useState("");
  const [showSettings, setShowSettings] = useState(false);
  const { theme, choice, setChoice } = useTheme();
  const searchRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  // 待解析队列：一次拖入多篇，或解析中又拖入新的，都排队逐篇处理
  const queue = useRef<string[]>([]);
  const busy = useRef(false);

  useEffect(() => {
    if (!isTauri) {
      const data = sample as Library;
      setPapers(data.papers ?? []);
      setActiveId(data.papers?.[0]?.id ?? "");
      setStatus("ready");
      return;
    }
    invoke<Settings>("get_settings").then(setSettings).catch(() => {});
    invoke<Library>("load_library")
      .then((data) => {
        setPapers(data.papers ?? []);
        setActiveId(data.papers?.[0]?.id ?? "");
        setStatus("ready");
      })
      .catch((e) => {
        setError(String(e));
        setStatus("error");
      });
  }, []);

  // 外部工具（WorkBuddy 等）写进收件箱的论文：启动时收一次，之后每 3 秒看一次
  useEffect(() => {
    if (!isTauri || status !== "ready") return;
    let alive = true;
    async function check() {
      try {
        const r = await invoke<ImportResult>("import_inbox");
        if (!alive) return;
        if (r.library) setPapers(r.library.papers);
        const parts = [];
        if (r.created.length) parts.push(`新增 ${r.created.length} 篇`);
        if (r.updated.length) parts.push(`更新 ${r.updated.length} 篇`);
        if (r.failed.length) parts.push(`${r.failed.length} 个文件格式有误`);
        if (parts.length) {
          const names = [...r.created, ...r.updated];
          setToast(`已从外部工具导入：${parts.join("，")}` + (names.length === 1 ? `《${names[0]}》` : ""));
          if (r.created.length) {
            setView("all");
            setTopic("");
            setQuery("");
          }
        }
      } catch {}
    }
    check();
    const t = setInterval(check, 3000);
    return () => {
      alive = false;
      clearInterval(t);
    };
  }, [status]);

  useEffect(() => {
    if (!toast) return;
    const t = setTimeout(() => setToast(""), 5000);
    return () => clearTimeout(t);
  }, [toast]);

  // ---------- 同步 ----------

  // 用 ref 而不是 state 判重入：同一个 tick 里连点两次时，state 还是旧值
  const syncBusy = useRef(false);

  async function runSync(silent = false) {
    if (syncBusy.current) return;
    syncBusy.current = true;
    setSyncing(true);
    setSyncStage("正在连接");
    try {
      const r = await invoke<SyncReport>("sync_now");
      if (r.library) setPapers(r.library.papers);
      setSync(await invoke<SyncSettings>("get_sync_settings"));
      // 静默同步（启动时自动跑）只在真有变化时提示，
      // 否则每次开应用都弹一句"已是最新"，很快就会变成噪音
      const changed = r.added || r.updated || r.deleted || r.pdf_up || r.pdf_down;
      if (!silent || changed) setToast(r.message);
    } catch (e) {
      setToast(String(e));
    } finally {
      syncBusy.current = false;
      setSyncing(false);
      setSyncStage("");
    }
  }

  // 启动时读同步配置；开了自动同步就先跑一次
  useEffect(() => {
    if (!isTauri) return;
    invoke<SyncSettings>("get_sync_settings")
      .then((s) => {
        setSync(s);
        if (s.enabled && s.auto) runSync(true);
      })
      .catch(() => {});
  }, []);

  // 同步进度：PDF 多的时候要跑几分钟，没有反馈用户会以为卡死
  useEffect(() => {
    if (!isTauri) return;
    const un = listen<{ detail?: string }>("summa://sync", (e) => {
      if (e.payload?.detail) setSyncStage(e.payload.detail);
    });
    return () => {
      un.then((off) => off()).catch(() => {});
    };
  }, []);

  function showError(name: string, e: unknown) {
    setJob({ name, stage: "操作失败", pct: 100, error: String(e), queued: 0 });
  }

  // ---------- 解析队列 ----------

  function enqueue(paths: string[]) {
    queue.current.push(...paths);
    if (!busy.current) runQueue();
  }

  async function runQueue() {
    busy.current = true;
    while (queue.current.length > 0) {
      const path = queue.current.shift()!;
      const ok = await handleFile(path);
      // 出错时停下来让用户看到原因；关闭错误框后继续剩下的
      if (!ok) {
        busy.current = false;
        return;
      }
    }
    busy.current = false;
    setJob(null);
  }

  async function handleFile(path: string): Promise<boolean> {
    const name = path.split(/[/\\]/).pop() || "paper.pdf";
    const step = (stage: string, pct: number) =>
      setJob({ name, stage, pct, error: "", queued: queue.current.length });
    step("读取文件", 6);
    try {
      const bytes = await invoke<ArrayBuffer>("read_pdf", { path });
      step("渲染页面", 12);

      const pages = await renderPdf(bytes, 12, (i, n) => {
        step(`渲染第 ${i} / ${n} 页`, 12 + Math.round((i / n) * 58));
      });

      step("模型解析中", 78);
      const paper = await invoke<Paper>("parse_pdf", { path, fileName: name, pages });

      // 重复拖入同一文件时 Rust 会沿用旧 id，这里同样先去掉旧的再放到最前
      setPapers((prev) => [paper, ...prev.filter((p) => p.id !== paper.id)]);
      setActiveId(paper.id);
      setQuery("");
      setTopic("");
      setView("all");
      return true;
    } catch (e) {
      const msg = String(e);
      setJob({
        name,
        stage: "解析失败",
        pct: 100,
        error: msg === "NO_KEY" ? "还没有配置 DeepSeek API Key，请先在设置里填写。" : msg,
        queued: queue.current.length,
      });
      if (msg === "NO_KEY") queue.current = [];
      return false;
    }
  }

  function closeError() {
    setJob(null);
    if (queue.current.length > 0) runQueue();
  }

  useEffect(() => {
    if (!isTauri) return;
    let unlisten: (() => void) | undefined;
    getCurrentWebview()
      .onDragDropEvent((event) => {
        const p = event.payload;
        if (p.type === "over" || p.type === "enter") {
          setDragging(true);
        } else if (p.type === "drop") {
          setDragging(false);
          const pdfs = p.paths.filter((x) => x.toLowerCase().endsWith(".pdf"));
          if (pdfs.length > 0) enqueue(pdfs);
        } else {
          setDragging(false);
        }
      })
      .then((fn) => {
        unlisten = fn;
      });
    return () => unlisten?.();
  }, []);

  // ---------- 派生数据 ----------

  const topics = useMemo(() => {
    const m = new Map<string, number>();
    papers.forEach((p) => p.tags.forEach((t) => m.set(t, (m.get(t) ?? 0) + 1)));
    return [...m.entries()].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0], "zh"));
  }, [papers]);

  const viewCounts = useMemo(
    () => Object.fromEntries(VIEWS.map((v) => [v.id, papers.filter((p) => inView(p, v.id)).length])),
    [papers]
  );

  const shown = useMemo(() => {
    const words = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
    const list = papers
      .map((p, i) => ({ p, i }))
      .filter(
        ({ p }) =>
          inView(p, view) && (!topic || p.tags.includes(topic)) && words.every((w) => haystack(p).includes(w))
      );
    list.sort((a, b) => {
      if (sort === "year") return (b.p.year || 0) - (a.p.year || 0) || a.i - b.i;
      if (sort === "title") return a.p.title.localeCompare(b.p.title, "zh");
      // 库里本来就是新的在前；旧记录没有 added 字段，保持原顺序
      return (b.p.added ?? 0) - (a.p.added ?? 0) || a.i - b.i;
    });
    return list.map(({ p }) => p);
  }, [papers, query, topic, view, sort]);

  const active = papers.find((p) => p.id === activeId) ?? null;

  // 筛选后当前文献不在列表里时，自动选中列表第一篇
  useEffect(() => {
    if (shown.length > 0 && !shown.some((p) => p.id === activeId)) setActiveId(shown[0].id);
  }, [shown]);

  // ---------- 键盘：↑↓ 切换文献，⌘F 搜索 ----------

  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "f") {
        e.preventDefault();
        searchRef.current?.focus();
        searchRef.current?.select();
        return;
      }
      const tag = (e.target as HTMLElement).tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || job || showSettings) return;
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "a") {
        e.preventDefault();
        setSelected(new Set(shown.map((p) => p.id)));
        return;
      }
      if (e.key === "Escape") {
        setSelected(new Set());
        return;
      }
      if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
      e.preventDefault();
      const i = shown.findIndex((p) => p.id === activeId);
      const next = shown[e.key === "ArrowDown" ? Math.min(i + 1, shown.length - 1) : Math.max(i - 1, 0)];
      if (next) {
        setActiveId(next.id);
        setSelected(new Set());
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [shown, activeId, job, showSettings]);

  // 当前文献滚动到可见范围
  useEffect(() => {
    listRef.current?.querySelector(".item.active")?.scrollIntoView({ block: "nearest" });
  }, [activeId]);

  // ---------- 修改 ----------

  async function patch(id: string, changes: PaperChanges) {
    setPapers((prev) => prev.map((p) => (p.id === id ? { ...p, ...changes } : p)));
    if (!isTauri) return;
    try {
      await invoke("update_paper", { id, patch: changes });
    } catch (e) {
      showError("保存修改", e);
    }
  }

  async function removePapers(ids: string[]) {
    if (ids.length === 0) return;
    try {
      if (isTauri) await invoke("delete_papers", { ids });
      const gone = new Set(ids);
      const i = shown.findIndex((p) => p.id === activeId);
      const next = shown.slice(i + 1).find((p) => !gone.has(p.id)) ?? shown.slice(0, i).reverse().find((p) => !gone.has(p.id));
      setPapers((prev) => prev.filter((p) => !gone.has(p.id)));
      setActiveId(next?.id ?? "");
      setSelected(new Set());
      if (ids.length > 1) setToast(`已删除 ${ids.length} 篇`);
    } catch (e) {
      showError("删除文献", e);
    }
  }

  function clickItem(e: MouseEvent, id: string) {
    if (e.metaKey || e.ctrlKey) {
      const next = new Set(selected.size ? selected : activeId ? [activeId] : []);
      next.has(id) ? next.delete(id) : next.add(id);
      setSelected(next);
      setActiveId(id);
      return;
    }
    if (e.shiftKey && activeId) {
      const a = shown.findIndex((p) => p.id === activeId);
      const b = shown.findIndex((p) => p.id === id);
      if (a >= 0 && b >= 0) {
        const [from, to] = a < b ? [a, b] : [b, a];
        setSelected(new Set(shown.slice(from, to + 1).map((p) => p.id)));
        return;
      }
    }
    setSelected(new Set());
    setActiveId(id);
  }

  async function bulkPatch(changes: PaperChanges | ((p: Paper) => PaperChanges)) {
    for (const p of papers.filter((p) => selected.has(p.id))) {
      await patch(p.id, typeof changes === "function" ? changes(p) : changes);
    }
  }

  async function removeTag(name: string) {
    setDeletingTag("");
    if (topic === name) setTopic("");
    if (!isTauri) {
      setPapers((prev) => prev.map((p) => ({ ...p, tags: p.tags.filter((t) => t !== name) })));
      return;
    }
    try {
      const lib = await invoke<Library>("delete_tag", { name });
      setPapers(lib.papers);
    } catch (e) {
      showError("删除主题", e);
    }
  }

  async function openActive() {
    if (!active) return;
    try {
      await invoke("open_paper", { id: active.id });
    } catch (e) {
      showError(active.title, e);
    }
  }

  async function commitRename() {
    if (!renaming) return;
    const { from, to } = renaming;
    setRenaming(null);
    const name = to.trim();
    if (!name || name === from) return;
    const apply = (list: Paper[]) =>
      list.map((p) =>
        p.tags.includes(from) ? { ...p, tags: [...new Set(p.tags.map((t) => (t === from ? name : t)))] } : p
      );
    if (isTauri) {
      try {
        const lib = await invoke<Library>("rename_tag", { from, to: name });
        setPapers(lib.papers);
      } catch (e) {
        showError("重命名主题", e);
        return;
      }
    } else {
      setPapers(apply);
    }
    if (topic === from) setTopic(name);
  }

  const noKey = isTauri && settings !== null && !settings.has_key;
  const listTitle = topic || VIEWS.find((v) => v.id === view)!.label;
  const filtered = view !== "all" || topic || query.trim();

  return (
    <div className="app">
      <header className="titlebar" data-tauri-drag-region>
        <div className="tb-title" data-tauri-drag-region>
          Summa
        </div>
        <div className="tb-search">
          <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5">
            <circle cx="7" cy="7" r="4.5" />
            <path d="M10.5 10.5L14 14" strokeLinecap="round" />
          </svg>
          <input
            ref={searchRef}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape") {
                setQuery("");
                e.currentTarget.blur();
              }
            }}
            placeholder="搜索题名、作者、方法、笔记…  ⌘F"
            spellCheck={false}
          />
        </div>
        <div className="tb-right">
          {!isTauri && <span className="preview-tag">浏览器预览模式</span>}
          <button
            className="tb-btn"
            title={theme === "dark" ? "切换到日间主题" : "切换到夜间主题"}
            onClick={() => setChoice(theme === "dark" ? "light" : "dark")}
          >
            {theme === "dark" ? (
              <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4">
                <circle cx="8" cy="8" r="3" />
                <path d="M8 1.5v1.6M8 12.9v1.6M1.5 8h1.6M12.9 8h1.6M3.4 3.4l1.1 1.1M11.5 11.5l1.1 1.1M3.4 12.6l1.1-1.1M11.5 4.5l1.1-1.1" strokeLinecap="round" />
              </svg>
            ) : (
              <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4">
                <path d="M13.5 9.6A5.6 5.6 0 0 1 6.4 2.5a5.6 5.6 0 1 0 7.1 7.1Z" strokeLinejoin="round" />
              </svg>
            )}
          </button>
          {isTauri && settings && (
            <button className="model-tag" onClick={() => setShowSettings(true)} title="设置">
              <i className={"dot" + (settings.has_key ? " ok" : " warn")} />
              {settings.has_key ? settings.model : "未配置 API Key"}
            </button>
          )}
        </div>
      </header>

      <div className="body">
        <aside className="sidebar">
          <div className="brand">
            <div className="logo">S</div>
            <div>
              <div className="brand-name">Summa</div>
              <div className="brand-sub">文渊 · {papers.length} 篇</div>
            </div>
          </div>

          <nav className="nav">
            {VIEWS.map((v) => (
              <div
                key={v.id}
                className={"nav-item" + (view === v.id && !topic ? " active" : "")}
                onClick={() => {
                  setView(v.id);
                  setTopic("");
                }}
              >
                {v.label} <span>{viewCounts[v.id]}</span>
              </div>
            ))}
          </nav>

          <div className="side-label" title="双击主题可重命名；改成已有的名字即合并">
            主题
          </div>
          <div className="topics">
            {topics.map(([name, count], i) =>
              renaming?.from === name ? (
                <input
                  key={name}
                  className="topic-input"
                  autoFocus
                  value={renaming.to}
                  onChange={(e) => setRenaming({ from: name, to: e.target.value })}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") commitRename();
                    if (e.key === "Escape") setRenaming(null);
                  }}
                  onBlur={commitRename}
                />
              ) : (
                <div
                  className={"topic" + (topic === name ? " active" : "")}
                  key={name}
                  title="双击重命名"
                  onClick={() => setTopic(topic === name ? "" : name)}
                  onDoubleClick={() => setRenaming({ from: name, to: name })}
                  onMouseLeave={() => deletingTag === name && setDeletingTag("")}
                >
                  <i style={{ background: COLORS[i % COLORS.length] }} />
                  <span className="topic-name">{name}</span>
                  {deletingTag === name ? (
                    <button
                      className="topic-del confirm"
                      onClick={(e) => {
                        e.stopPropagation();
                        removeTag(name);
                      }}
                    >
                      移除主题？
                    </button>
                  ) : (
                    <>
                      <em>{count}</em>
                      <button
                        className="topic-del"
                        title="删除这个主题（文献不会被删除）"
                        onClick={(e) => {
                          e.stopPropagation();
                          setDeletingTag(name);
                        }}
                      >
                        ×
                      </button>
                    </>
                  )}
                </div>
              )
            )}
            {topics.length === 0 && <div className="topic empty">暂无主题</div>}
          </div>

          <div className="local-card">
            {isTauri && sync?.enabled && (
              <div
                className="nav-item sync-item"
                onClick={() => runSync()}
                title={syncing ? "正在同步" : "点击立即同步"}
              >
                同步
                <span className={"sync-state" + (syncing ? " busy" : "")}>
                  {syncing ? syncStage || "同步中…" : lastSyncText(sync.last_sync)}
                </span>
              </div>
            )}
            {isTauri && (
              <div className="nav-item" onClick={() => setShowSettings(true)}>
                设置
                {noKey && <span className="warn-t">需要配置</span>}
              </div>
            )}
            <div className="local-inner">
              <span className={"dot" + (noKey ? " warn" : " ok")} />
              <div>
                <div className="local-t">拖入 PDF 开始解析</div>
                <div className="local-s">可一次拖入多篇。PDF 原件保存在本机，只上传前 12 页的页面图像</div>
              </div>
            </div>
          </div>
        </aside>

        <section className="list">
          <div className="list-head">
            <div className="list-title">
              <h2>{listTitle}</h2>
              <span>{shown.length === papers.length ? `${papers.length} 篇` : `${shown.length} / ${papers.length} 篇`}</span>
            </div>
            <div className="list-tools">
              <select value={sort} onChange={(e) => setSort(e.target.value as Sort)}>
                <option value="added">最近添加</option>
                <option value="year">年份（新→旧）</option>
                <option value="title">题名（A→Z）</option>
              </select>
              {filtered && (
                <button
                  className="link-btn"
                  onClick={() => {
                    setView("all");
                    setTopic("");
                    setQuery("");
                  }}
                >
                  清除筛选
                </button>
              )}
            </div>
          </div>
          <div className="items" ref={listRef}>
            {shown.map((p) => (
              <article
                key={p.id}
                className={
                  "item" +
                  (selected.size > 1 ? (selected.has(p.id) ? " active" : "") : p.id === activeId ? " active" : "") +
                  (p.read ? " is-read" : "")
                }
                onClick={(e) => clickItem(e, p.id)}
              >
                <p className="item-t">
                  {!p.read && <i className="unread" title="待读" />}
                  {p.title}
                </p>
                <div className="item-m">
                  {p.starred && <span className="star">★</span>}
                  {p.authors.split(",")[0]} · {p.venue} · {p.year || "—"}
                </div>
                <div className="item-tags">
                  {p.tags.map((t) => (
                    <span className="chip" key={t}>
                      {t}
                    </span>
                  ))}
                </div>
              </article>
            ))}
            {shown.length === 0 && papers.length > 0 && <div className="list-empty">没有匹配的文献</div>}
            {shown.length > 1 && selected.size < 2 && <div className="list-tip">⌘ 点击多选 · Shift 点击连选 · ⌘A 全选</div>}
          </div>
        </section>

        <main className="detail">
          {status === "loading" && <div className="center-hint">正在读取文献库…</div>}
          {status === "error" && (
            <div className="center-hint err">
              <b>读取失败</b>
              <span>{error}</span>
            </div>
          )}
          {status === "ready" && selected.size > 1 && (
            <BulkPanel
              papers={papers.filter((p) => selected.has(p.id))}
              allTopics={topics.map(([t]) => t)}
              canDelete={isTauri}
              onPatch={bulkPatch}
              onDelete={() => removePapers([...selected])}
              onClear={() => setSelected(new Set())}
            />
          )}
          {status === "ready" && selected.size < 2 && active && (
            <Detail
              paper={active}
              allTopics={topics.map(([t]) => t)}
              canFile={isTauri}
              onPatch={(c) => patch(active.id, c)}
              onOpen={openActive}
              onDelete={() => removePapers([active.id])}
            />
          )}
          {status === "ready" && !active && (
            <div className="center-hint">
              {noKey ? (
                <>
                  <b>第一步：配置 DeepSeek API Key</b>
                  <span>Summa 用 DeepSeek 读论文页面，需要你自己的 Key</span>
                  <button className="btn primary" onClick={() => setShowSettings(true)}>
                    打开设置
                  </button>
                </>
              ) : (
                <>
                  <b>{papers.length === 0 ? "把 PDF 拖进窗口" : "从左侧选择一篇文献"}</b>
                  <span>Summa 会渲染页面图像、调用模型解析，并归入主题</span>
                </>
              )}
            </div>
          )}
        </main>
      </div>

      {toast && (
        <div className="toast" onClick={() => setToast("")}>
          {toast}
        </div>
      )}

      {dragging && (
        <div className="dropzone">
          <div className="dz-box">
            <div className="dz-t">松开以解析</div>
            <div className="dz-s">自动渲染页面、生成要义页并归入主题</div>
          </div>
        </div>
      )}

      {showSettings && settings && (
        <SettingsDialog
          settings={settings}
          sync={sync}
          syncing={syncing}
          theme={choice}
          onTheme={setChoice}
          onSaved={setSettings}
          onSyncSaved={setSync}
          onSync={() => runSync()}
          onClose={() => setShowSettings(false)}
        />
      )}

      {job && (
        <div className="overlay">
          <div className="job">
            <div className="job-head">
              <div>
                <div className="job-name">{job.name}</div>
                <div className={"job-stage" + (job.error ? " bad" : "")}>
                  {job.stage}
                  {job.queued > 0 && ` · 还有 ${job.queued} 篇排队`}
                </div>
              </div>
              {!job.error && <div className="job-pct">{job.pct}%</div>}
            </div>
            <div className="job-bar">
              <i style={{ width: `${job.pct}%` }} />
            </div>
            {job.error && (
              <>
                <pre className="job-err">{job.error}</pre>
                <div className="job-btns">
                  {noKey && (
                    <button
                      className="btn primary"
                      onClick={() => {
                        setJob(null);
                        setShowSettings(true);
                      }}
                    >
                      打开设置
                    </button>
                  )}
                  <button className="btn" onClick={closeError}>
                    {job.queued > 0 ? "跳过，继续下一篇" : "关闭"}
                  </button>
                </div>
              </>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
