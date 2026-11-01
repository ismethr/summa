import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { Folder, Paper, Placement } from "./types";
import { folderRows } from "./folders";

export default function OrganizeDialog({ papers, folders, selected, canAI, onApply, onClose }: {
  papers: Paper[]; folders: Folder[]; selected: Set<string>; canAI: boolean;
  onApply: (plan: Placement[]) => Promise<void>; onClose: () => void;
}) {
  const [scope, setScope] = useState(selected.size > 1 ? "selected" : "unfiled");
  const [plan, setPlan] = useState<Placement[] | null>(null);
  const [paths, setPaths] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const liveIds = new Set(folderRows(folders).map((r) => r.folder.id));
  const candidates = papers.filter((p) => scope === "all" || (scope === "selected" ? selected.has(p.id) : !liveIds.has(p.folder_id ?? "")));
  const targets = candidates.slice(0, 200);
  const editedPlan = plan?.map((item) => ({ ...item, path: (paths[item.paper_id] ?? item.path.join(" / ")).split("/").map((s) => s.trim()) }));
  async function generate() {
    setBusy(true); setError("");
    try {
      setPlan(await invoke<Placement[]>("suggest_folders", { ids: targets.map((p) => p.id) }));
      setPaths({});
    }
    catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  }
  async function apply() {
    if (!editedPlan) return;
    setBusy(true); setError("");
    try { await onApply(editedPlan); onClose(); }
    catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  }
  return <div className="overlay" onKeyDown={(e) => { e.stopPropagation(); if (e.key === "Escape" && !busy) onClose(); }}>
    <div className="folder-dialog organize-dialog" role="dialog" aria-modal="true" aria-labelledby="organize-title">
      <h2 id="organize-title">AI 整理文献</h2>
      {!plan ? <>
        <p className="folder-hint">根据研究主题建议目录层级与文献归属。生成后可修改路径，再应用到文献库。</p>
        <label>整理范围<select autoFocus disabled={busy} value={scope} onChange={(e) => setScope(e.target.value)}>
          <option value="unfiled">未归档文献</option><option value="all">全部文献（会重新分配归属）</option>
          {selected.size > 1 && <option value="selected">所选文献</option>}
        </select></label>
        <p className="folder-hint">本次 {targets.length} 篇{candidates.length > 200 ? `，共 ${candidates.length} 篇；每批最多 200 篇，建议选择「未归档文献」分批整理` : ""}。将向 DeepSeek 发送这些文献的题名、主题、要旨、研究问题及现有目录名称，使用已配置的 API Key 并产生调用费用。</p>
        {!canAI && <p className="folder-hint">请在桌面应用中配置 DeepSeek API Key 后使用 AI 整理。</p>}
      </> : <>
        <p className="folder-hint">已为 {plan.length} 篇文献生成方案。路径使用 / 分隔层级（最多 6 层），同名路径会复用已有文件夹。</p>
        <div className="organize-preview">{plan.map((item, i) => <label key={item.paper_id}>
          <span>{papers.find((p) => p.id === item.paper_id)?.title ?? item.paper_id}</span>
          <input aria-label={`第 ${i + 1} 篇文献的目录路径`} value={paths[item.paper_id] ?? item.path.join(" / ")} onChange={(e) => setPaths({ ...paths, [item.paper_id]: e.target.value })} disabled={busy} />
        </label>)}</div>
      </>}
      {error && <p className="folder-error" role="alert">{error}</p>}
      <div className="folder-dialog-actions">
        <button className="btn" disabled={busy} onClick={onClose}>取消</button>
        {plan && <button className="btn" disabled={busy} onClick={() => setPlan(null)}>重新生成</button>}
        <button className="btn primary" disabled={busy || !canAI || (!plan && !targets.length) || !!editedPlan?.some((p) => !p.path.length || p.path.length > 6 || p.path.some((s) => !s || [...s].length > 60 || /\p{Cc}/u.test(s)))} onClick={plan ? apply : generate}>
          {busy ? (plan ? "应用中…" : "正在生成方案…") : plan ? "应用整理方案" : "生成整理方案"}
        </button>
      </div>
    </div>
  </div>;
}
