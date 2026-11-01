import assert from "node:assert/strict";
import { test } from "node:test";
import { folderRows, folderDescendants, inFolder } from "../.test-dist/folders.js";

const folder = (id, name, parent_id = "", deleted = false) => ({ id, name, parent_id, deleted, rev: 1 });
const folders = [folder("a", "课题"), folder("b", "方法", "a"), folder("c", "模型", "b"), folder("d", "另一个课题")];

test("树按层级展开，路径完整，父目录包含全部后代", () => {
  const rows = folderRows(folders);
  assert.equal(rows.find((r) => r.folder.id === "c").path, "课题 / 方法 / 模型");
  assert.equal(rows.find((r) => r.folder.id === "c").depth, 2);
  assert.deepEqual([...folderDescendants(rows, "a")], ["a", "b", "c"]);
  assert.ok(inFolder({ folder_id: "c" }, "a", rows));
  assert.equal(inFolder({ folder_id: "d" }, "a", rows), false);
});

test("旧数据、已删除目录和缺失目录中的文献归入未归档", () => {
  const rows = folderRows([...folders, folder("deleted", "已删除", "", true)]);
  for (const paper of [{}, { folder_id: "" }, { folder_id: "missing" }, { folder_id: "deleted" }]) {
    assert.ok(inFolder(paper, "__unfiled__", rows));
    assert.ok(inFolder(paper, "", rows));
  }
  assert.equal(inFolder({ folder_id: "c" }, "__unfiled__", rows), false);
});

test("空目录可见，已删除目录不可见", () => {
  const rows = folderRows([folder("empty", "空目录"), folder("gone", "已删除", "", true)]);
  assert.deepEqual(rows.map((r) => r.folder.id), ["empty"]);
});

test("缺失父目录和同步造成的环都能安全呈现，目录不丢失", () => {
  const rows = folderRows([
    folder("a", "A", "b"), folder("b", "B", "a"),
    folder("self", "自身", "self"), folder("orphan", "孤立", "missing"),
    folder("child", "子级", "orphan"),
  ]);
  assert.equal(rows.length, 5);
  assert.ok(rows.every((r) => r.depth === 0));
  assert.equal(new Set(rows.map((r) => r.folder.id)).size, 5);
});

test("文件夹移动后后代筛选使用新层级", () => {
  const rows = folderRows(folders.map((f) => f.id === "b" ? { ...f, parent_id: "d" } : f));
  assert.deepEqual([...folderDescendants(rows, "a")], ["a"]);
  assert.deepEqual([...folderDescendants(rows, "d")], ["d", "b", "c"]);
});
