import { useEffect, useState } from "react";
import type { Paper } from "./types";

export type PaperChanges = Partial<Pick<Paper, "tags" | "read" | "starred" | "note">>;

type Props = {
  paper: Paper;
  allTopics: string[];
  canFile: boolean;
  onPatch: (changes: PaperChanges) => void;
  onOpen: () => void;
  onDelete: () => void;
};

/** 简化的 APA 格式：作者 (年份). 题名. 期刊. */
export function citation(p: Paper) {
  const parts = [
    p.authors.trim() || "佚名",
    ` (${p.year || "n.d."}). `,
    p.title.trim().replace(/[.。]$/, ""),
    ".",
    p.venue.trim() ? ` ${p.venue.trim()}.` : "",
  ];
  return parts.join("");
}

export default function Detail({ paper, allTopics, canFile, onPatch, onOpen, onDelete }: Props) {
  const [note, setNote] = useState(paper.note ?? "");
  const [adding, setAdding] = useState(false);
  const [newTag, setNewTag] = useState("");
  const [copied, setCopied] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);

  // 切换文献时重置本地编辑状态
  useEffect(() => {
    setNote(paper.note ?? "");
    setAdding(false);
    setNewTag("");
    setCopied(false);
    setConfirmDelete(false);
  }, [paper.id]);

  // 笔记停止输入 0.8 秒后自动保存
  useEffect(() => {
    if (note === (paper.note ?? "")) return;
    const t = setTimeout(() => onPatch({ note }), 800);
    return () => clearTimeout(t);
  }, [note]);

  function addTag() {
    const t = newTag.trim();
    if (t && !paper.tags.includes(t)) onPatch({ tags: [...paper.tags, t] });
    setNewTag("");
    setAdding(false);
  }

  async function copyCitation() {
    try {
      await navigator.clipboard.writeText(citation(paper));
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {}
  }

  const hasSteps = paper.steps.length > 0;
  const hasFindings = paper.findings.length > 0;

  return (
    <div className="dwrap">
      <div className="kicker">要义页 · Synopsis</div>
      <h1 className="d-h1">{paper.title}</h1>
      <div className="byline">
        <b>{paper.authors}</b> · {paper.venue} {paper.year || ""}
        {paper.doi && <> · DOI {paper.doi}</>}
        {paper.origin && <span className="origin">来自 {paper.origin}</span>}
      </div>

      <div className="d-actions">
        <button className={"btn icon-btn" + (paper.starred ? " on-star" : "")} onClick={() => onPatch({ starred: !paper.starred })}>
          {paper.starred ? "★ 已星标" : "☆ 星标"}
        </button>
        <button className={"btn icon-btn" + (paper.read ? " on-read" : "")} onClick={() => onPatch({ read: !paper.read })}>
          {paper.read ? "✓ 已读" : "标为已读"}
        </button>
        <button className="btn" onClick={copyCitation}>
          {copied ? "已复制" : "复制引用"}
        </button>
        {canFile && (paper.file || paper.source) && (
          <button className="btn" onClick={onOpen}>
            打开 PDF
          </button>
        )}
        {canFile && (
          <button
            className={"btn push-right" + (confirmDelete ? " danger" : "")}
            onClick={() => (confirmDelete ? onDelete() : setConfirmDelete(true))}
            onMouseLeave={() => setConfirmDelete(false)}
          >
            {confirmDelete ? "确认删除？" : "删除"}
          </button>
        )}
      </div>

      <div className="d-tags">
        {paper.tags.map((t) => (
          <span className="chip tag-edit" key={t}>
            {t}
            <button title="移除主题" onClick={() => onPatch({ tags: paper.tags.filter((x) => x !== t) })}>
              ×
            </button>
          </span>
        ))}
        {adding ? (
          <>
            <input
              className="tag-input"
              autoFocus
              list="all-topics"
              value={newTag}
              placeholder="主题名，回车确认"
              onChange={(e) => setNewTag(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") addTag();
                if (e.key === "Escape") setAdding(false);
              }}
              onBlur={addTag}
            />
            <datalist id="all-topics">
              {allTopics
                .filter((t) => !paper.tags.includes(t))
                .map((t) => (
                  <option key={t} value={t} />
                ))}
            </datalist>
          </>
        ) : (
          <button className="chip tag-add" onClick={() => setAdding(true)}>
            + 主题
          </button>
        )}
      </div>

      {paper.gist && <div className="gist">{paper.gist}</div>}

      {paper.problem && (
        <section>
          <h2 className="sec">研究问题</h2>
          <p className="sec-p">{paper.problem}</p>
        </section>
      )}

      {hasSteps && (
        <section>
          <h2 className="sec">方法路径</h2>
          <ol className="steps">
            {paper.steps.map((s, i) => (
              <li key={i}>
                <span className="num">{i + 1}</span>
                <p>
                  <b>{s.name}。</b>
                  {s.detail}
                </p>
              </li>
            ))}
          </ol>
        </section>
      )}

      {hasFindings && (
        <section>
          <h2 className="sec">关键发现</h2>
          <div className="findings">
            {paper.findings.map((f, i) => (
              <div className="finding" key={i}>
                <span className="f-num">{f.value || "—"}</span>
                <span className="f-lbl">{f.label}</span>
              </div>
            ))}
          </div>
        </section>
      )}

      {paper.limits && (
        <section>
          <h2 className="sec">局限与适用边界</h2>
          <p className="sec-p">{paper.limits}</p>
        </section>
      )}

      <section className="note-sec">
        <h2 className="sec">
          我的笔记
          {note !== (paper.note ?? "") && <span className="saving">保存中…</span>}
        </h2>
        <textarea
          className="note"
          value={note}
          placeholder="写下你的想法、可借鉴的点、和自己研究的关系…（自动保存）"
          onChange={(e) => setNote(e.target.value)}
          onBlur={() => note !== (paper.note ?? "") && onPatch({ note })}
        />
      </section>
    </div>
  );
}
