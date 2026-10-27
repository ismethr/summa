import { useState } from "react";
import type { Paper } from "./types";
import type { PaperChanges } from "./Detail";

type Props = {
  papers: Paper[];
  allTopics: string[];
  canDelete: boolean;
  onPatch: (changes: PaperChanges | ((p: Paper) => PaperChanges)) => void;
  onDelete: () => void;
  onClear: () => void;
};

/** 多选时右侧显示的批量操作面板 */
export default function BulkPanel({ papers, allTopics, canDelete, onPatch, onDelete, onClear }: Props) {
  const [tag, setTag] = useState("");
  const [confirmDelete, setConfirmDelete] = useState(false);
  const n = papers.length;

  function addTag() {
    const t = tag.trim();
    if (!t) return;
    onPatch((p) => ({ tags: p.tags.includes(t) ? p.tags : [...p.tags, t] }));
    setTag("");
  }

  return (
    <div className="dwrap">
      <div className="kicker">批量操作</div>
      <h1 className="d-h1">已选 {n} 篇文献</h1>
      <div className="byline">
        <button className="link-btn" onClick={onClear}>
          取消选择（Esc）
        </button>
      </div>

      <section>
        <h2 className="sec">阅读状态</h2>
        <div className="bulk-row">
          <button className="btn" onClick={() => onPatch({ read: true })}>
            全部标为已读
          </button>
          <button className="btn" onClick={() => onPatch({ read: false })}>
            全部标为待读
          </button>
          <button className="btn" onClick={() => onPatch({ starred: true })}>
            ★ 全部星标
          </button>
          <button className="btn" onClick={() => onPatch({ starred: false })}>
            取消星标
          </button>
        </div>
      </section>

      <section>
        <h2 className="sec">添加主题</h2>
        <div className="bulk-row">
          <input
            className="tag-input wide"
            list="bulk-topics"
            value={tag}
            placeholder="输入或选择主题，回车添加到所选文献"
            onChange={(e) => setTag(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && addTag()}
          />
          <datalist id="bulk-topics">
            {allTopics.map((t) => (
              <option key={t} value={t} />
            ))}
          </datalist>
          <button className="btn" onClick={addTag} disabled={!tag.trim()}>
            添加
          </button>
        </div>
      </section>

      {canDelete && (
        <section>
          <h2 className="sec">删除</h2>
          <p className="sec-p">删除所选文献的记录和库里保存的 PDF 副本，你电脑上原来的 PDF 文件不受影响。此操作不能撤销。</p>
          <div className="bulk-row" style={{ marginTop: 12 }}>
            <button
              className={"btn" + (confirmDelete ? " danger" : "")}
              onClick={() => (confirmDelete ? onDelete() : setConfirmDelete(true))}
              onMouseLeave={() => setConfirmDelete(false)}
            >
              {confirmDelete ? `确认删除 ${n} 篇？` : `删除这 ${n} 篇`}
            </button>
          </div>
        </section>
      )}

      <section>
        <h2 className="sec">所选文献</h2>
        <ul className="bulk-list">
          {papers.map((p) => (
            <li key={p.id}>{p.title}</li>
          ))}
        </ul>
      </section>
    </div>
  );
}
