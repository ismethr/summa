import type { Folder, Paper } from "./types";

export type FolderRow = { folder: Folder; depth: number; path: string; parentId: string };

/** 丢失父级、并发移动成环时将相关目录置于根层，保证目录及文献可访问。 */
export function folderRows(folders: Folder[]): FolderRow[] {
  const live = folders.filter((f) => !f.deleted);
  const byId = new Map(live.map((f) => [f.id, f]));
  const parents = new Map<string, string>();
  for (const f of live) {
    const seen = new Set([f.id]);
    let parent = f.parent_id;
    let invalid = false;
    while (parent) {
      if (seen.has(parent) || !byId.has(parent)) { invalid = true; break; }
      seen.add(parent);
      parent = byId.get(parent)!.parent_id;
    }
    parents.set(f.id, invalid ? "" : f.parent_id);
  }
  const children = new Map<string, Folder[]>();
  for (const f of live) {
    const parent = parents.get(f.id)!;
    children.set(parent, [...(children.get(parent) ?? []), f]);
  }
  for (const list of children.values()) list.sort((a, b) => a.name.localeCompare(b.name, "zh") || a.id.localeCompare(b.id));
  const rows: FolderRow[] = [];
  function visit(parentId: string, depth: number, prefix: string) {
    for (const folder of children.get(parentId) ?? []) {
      const path = prefix ? `${prefix} / ${folder.name}` : folder.name;
      rows.push({ folder, depth, path, parentId });
      visit(folder.id, depth + 1, path);
    }
  }
  visit("", 0, "");
  return rows;
}

export function folderDescendants(rows: FolderRow[], id: string): Set<string> {
  const ids = new Set([id]);
  for (const row of rows) if (ids.has(row.parentId)) ids.add(row.folder.id);
  return ids;
}

export function inFolder(paper: Paper, id: string, rows: FolderRow[]): boolean {
  if (!id) return true;
  if (id === "__unfiled__") return !rows.some((r) => r.folder.id === paper.folder_id);
  return folderDescendants(rows, id).has(paper.folder_id ?? "");
}
