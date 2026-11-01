import type { Folder } from "./types";
import { folderRows } from "./folders";

export default function FolderPicker({ folders, value, onChange, bulk = false }: {
  folders: Folder[]; value: string; onChange: (id: string) => void; bulk?: boolean;
}) {
  return <label className="folder-picker">{bulk ? "移动所选文献到" : "所在文件夹"}
    <select aria-label={bulk ? "移动所选文献到文件夹" : "文献所在文件夹"} value={value} onChange={(e) => onChange(e.target.value)}>
      {bulk && <option value="__choose__" disabled>选择文件夹…</option>}
      <option value="">未归档</option>
      {folderRows(folders).map((r) => <option key={r.folder.id} value={r.folder.id}>{r.path}</option>)}
    </select>
  </label>;
}
