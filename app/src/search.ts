import type { Paper } from "./types";

/** 同时规范化文献和查询，兼容大小写、全角字符及连续空白。 */
export function normalizeSearchText(text: string): string {
  return text.normalize("NFKC").toLowerCase().replace(/\s+/gu, " ").trim();
}

/** 只索引可供阅读的字段，避免本机文件路径参与匹配。 */
export function paperSearchText(paper: Paper): string {
  return normalizeSearchText([
    paper.title, paper.authors, paper.venue, paper.year || "",
    paper.doi, paper.origin, paper.gist, paper.problem, paper.limits, paper.note,
    ...paper.tags,
    ...paper.steps.flatMap((step) => [step.name, step.detail]),
    ...paper.findings.flatMap((finding) => [finding.value, finding.label]),
  ].filter((value) => value !== undefined).join(" "));
}

export function searchWords(query: string): string[] {
  return normalizeSearchText(query).split(" ").filter(Boolean);
}

export function matchesSearch(text: string, words: readonly string[]): boolean {
  return words.every((word) => text.includes(word));
}
