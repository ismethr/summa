import { useMemo, useState } from "react";
import type { Folder, Paper } from "./types";
import { folderDescendants, folderRows, inFolder } from "./folders";

type Props = {
  folders: Folder[]; papers: Paper[]; active: string;
  onSelect: (id: string) => void;
  onSave: (id: string | null, name: string, parentId: string) => Promise<void>;
  onDelete: (id: string) => Promise<void>;
  onAI: () => void;
};

export default function FolderSidebar({ folders, papers, active, onSelect, onSave, onDelete, onAI }: Props) {
  const rows = useMemo(() => folderRows(folders), [folders]);
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const [editor, setEditor] = useState<{ id: string | null; name: string; parentId: string } | null>(null);
  const [error, setError] = useState("");
  const [saving, setSaving] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const blocked = useMemo(() => editor?.id ? folderDescendants(rows, editor.id) : new Set<string>(), [rows, editor?.id]);
  const hidden = new Set<string>();
  const visible = rows.filter((row) => {
    if (hidden.has(row.parentId) || collapsed.has(row.parentId)) { hidden.add(row.folder.id); return false; }
    return true;
  });
  function open(id: string | null, name: string, parentId: string) {
    setEditor({ id, name, parentId }); setError(""); setConfirmDelete(false);
  }
  async function save() {
    if (!editor || saving) return;
    setSaving(true); setError("");
    try {
      await onSave(editor.id, editor.name, editor.parentId);
      setCollapsed((prev) => { const next = new Set(prev); next.delete(editor.parentId); return next; });
      setEditor(null);
    } catch (e) { setError(String(e)); }
    finally { setSaving(false); }
  }
  async function remove() {
    if (!editor?.id || saving) return;
    if (!confirmDelete) { setConfirmDelete(true); return; }
    setSaving(true); setError("");
    try { await onDelete(editor.id); setEditor(null); }
    catch (e) { setError(String(e)); }
    finally { setSaving(false); }
  }
  return <>
    <div className="side-label folder-heading">
      <span>文件夹</span>
      <button className="link-btn" onClick={onAI} title="AI 建议目录和文献归属">AI 整理</button>
      <button className="folder-icon" aria-label="新建文件夹" onClick={() => open(null, "", "")}>＋</button>
    </div>
    <nav className="folder-tree" aria-label="文献文件夹">
      <button className={"folder-select unfiled" + (active === "__unfiled__" ? " active" : "")} onClick={() => onSelect("__unfiled__")}>
        <span>未归档</span><em>{papers.filter((p) => inFolder(p, "__unfiled__", rows)).length}</em>
      </button>
      {visible.map(({ folder: f, depth, path }) => {
        const children = rows.some((r) => r.parentId === f.id);
        const descendants = folderDescendants(rows, f.id);
        return <div className={"folder-row" + (active === f.id ? " active" : "")} key={f.id} style={{ paddingLeft: Math.min(depth, 8) * 12 }}>
          <button className="folder-icon folder-toggle" disabled={!children} aria-label={`${collapsed.has(f.id) ? "展开" : "折叠"}${f.name}`} aria-expanded={children ? !collapsed.has(f.id) : undefined} onClick={() => setCollapsed((prev) => {
            const next = new Set(prev); next.has(f.id) ? next.delete(f.id) : next.add(f.id); return next;
          })}>{children ? (collapsed.has(f.id) ? "▸" : "▾") : "·"}</button>
          <button className="folder-select" title={path} onClick={() => onSelect(f.id)}>
            <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor"><path d="M1.5 4V3h5l1.5 2h6.5v8h-13z" /></svg>
            <span>{f.name}</span><em>{papers.filter((p) => descendants.has(p.folder_id ?? "")).length}</em>
          </button>
          <button className="folder-icon folder-menu" aria-label={`管理文件夹${f.name}`} title="改名、移动或删除" onClick={() => open(f.id, f.name, f.parent_id)}>⋯</button>
          <button className="folder-icon folder-menu" aria-label={`在${f.name}中新建子文件夹`} title="新建子文件夹" onClick={() => open(null, "", f.id)}>＋</button>
        </div>;
      })}
      {!rows.length && <p className="folder-hint">按课题或项目创建多级目录</p>}
    </nav>
    {editor && <div className="overlay" onKeyDown={(e) => { e.stopPropagation(); if (e.key === "Escape" && !saving) setEditor(null); }}>
      <form className="folder-dialog" role="dialog" aria-modal="true" aria-labelledby="folder-dialog-title" onSubmit={(e) => { e.preventDefault(); save(); }}>
        <h2 id="folder-dialog-title">{editor.id ? "管理文件夹" : "新建文件夹"}</h2>
        <label>名称<input autoFocus disabled={saving} maxLength={60} value={editor.name} onChange={(e) => { setEditor({ ...editor, name: e.target.value }); setConfirmDelete(false); }} /></label>
        <label>上级文件夹<select disabled={saving} value={editor.parentId} onChange={(e) => { setEditor({ ...editor, parentId: e.target.value }); setConfirmDelete(false); }}>
          <option value="">根目录</option>
          {rows.filter((r) => !blocked.has(r.folder.id)).map((r) => <option key={r.folder.id} value={r.folder.id}>{r.path}</option>)}
        </select></label>
        {editor.id && <p className="folder-hint">删除此文件夹及子文件夹后，其中的文献会保留在「未归档」。</p>}
        {error && <p className="folder-error" role="alert">{error}</p>}
        <div className="folder-dialog-actions">
          {editor.id && <button type="button" className="btn danger" disabled={saving} onClick={remove}>{confirmDelete ? "确认删除目录？" : "删除目录"}</button>}
          <button type="button" className="btn" disabled={saving} onClick={() => setEditor(null)}>取消</button>
          <button className="btn primary" disabled={saving || !editor.name.trim()}>{saving ? "保存中…" : "保存"}</button>
        </div>
      </form>
    </div>}
  </>;
}
