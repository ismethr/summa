import assert from "node:assert/strict";
import { test } from "node:test";
import { matchesSearch, paperSearchText, searchWords } from "../.test-dist/search.js";

const paper = {
  id: "p1", title: "ＡＩ Paper", authors: "张三", venue: "Nature", year: 2026,
  tags: ["机器学习"], gist: "研究要旨", problem: "数据稀缺问题",
  steps: [{ name: "训练", detail: "使用对比学习" }],
  findings: [{ value: "97.3%", label: "分类准确率" }],
  limits: "缺少外部验证", note: "考虑用于课题", doi: "10.1234/EXAMPLE",
  origin: "workbuddy", file: "/private/library.pdf", source: "/downloads/input.pdf",
};

test("检索覆盖全部可阅读字段", () => {
  const text = paperSearchText(paper);
  for (const query of [
    "ai paper", "张三", "nature", "2026", "机器学习", "研究要旨",
    "数据稀缺问题", "训练", "对比学习", "97.3%", "分类准确率",
    "外部验证", "课题", "10.1234/example", "workbuddy",
  ]) {
    assert.ok(matchesSearch(text, searchWords(query)), query);
  }
});

test("多关键词跨字段匹配，必须全部命中", () => {
  const text = paperSearchText(paper);
  assert.ok(matchesSearch(text, searchWords("张三 对比学习 97.3%")));
  assert.equal(matchesSearch(text, searchWords("张三 未知主题")), false);
});

test("查询与文献采用相同的全角、大小写与空白规范化", () => {
  assert.ok(matchesSearch(paperSearchText(paper), searchWords("  ＡＩ\tＰＡＰＥＲ　ＮＡＴＵＲＥ\n")));
  assert.ok(matchesSearch(paperSearchText({ ...paper, title: "multi\n line\t title" }), searchWords("multi line title")));
});

test("空查询保留全部文献", () => {
  assert.deepEqual(searchWords(" \n\t　"), []);
  assert.ok(matchesSearch(paperSearchText(paper), searchWords("")));
});

test("本地路径不进入索引", () => {
  const text = paperSearchText(paper);
  for (const query of ["private", "downloads", "library.pdf", "input.pdf"]) {
    assert.equal(matchesSearch(text, searchWords(query)), false);
  }
});

test("旧记录允许缺少可选字段，未知年份不作为 0 索引", () => {
  const { note, doi, origin, ...legacy } = paper;
  const text = paperSearchText({ ...legacy, year: 0, findings: [] });
  assert.equal(text.includes("undefined"), false);
  assert.equal(text.includes("null"), false);
  assert.equal(matchesSearch(text, ["0"]), false);
});

test("文献更新后索引反映最新笔记和主题", () => {
  const updated = paperSearchText({ ...paper, note: "新笔记", tags: ["新主题"] });
  assert.ok(matchesSearch(updated, searchWords("新笔记 新主题")));
  assert.equal(matchesSearch(updated, searchWords("课题")), false);
  assert.equal(matchesSearch(updated, searchWords("机器学习")), false);
});
