import * as pdfjsLib from "pdfjs-dist";
import workerUrl from "pdfjs-dist/build/pdf.worker.min.mjs?url";

pdfjsLib.GlobalWorkerOptions.workerSrc = workerUrl;

export type RenderProgress = (done: number, total: number) => void;

/**
 * 把 PDF 逐页渲染成 JPEG base64。
 *
 * 宽度固定 1400px —— 约合 150 DPI，是双栏论文的可用下限。
 * 低于此值，栏间距会糊连，模型会把左栏末尾和右栏开头读成一句话。
 */
export async function renderPdf(
  pdfBytes: ArrayBuffer,
  maxPages = 12,
  onProgress?: RenderProgress
): Promise<string[]> {
  const task = pdfjsLib.getDocument({ data: new Uint8Array(pdfBytes) });
  const pages: string[] = [];
  // 所有页共用一块画布，避免每页新建 1400×2000 的位图
  const canvas = document.createElement("canvas");

  try {
    const doc = await task.promise;
    const total = Math.min(doc.numPages, maxPages);

    for (let i = 1; i <= total; i++) {
      const page = await doc.getPage(i);
      const base = page.getViewport({ scale: 1 });
      const scale = 1400 / base.width;
      const viewport = page.getViewport({ scale });

      canvas.width = Math.floor(viewport.width);
      canvas.height = Math.floor(viewport.height);
      const ctx = canvas.getContext("2d");
      if (!ctx) throw new Error("无法创建画布上下文");

      // v6 的 RenderParameters 里 canvas 是必需参数，同时传 canvasContext 会冲突
      await page.render({ canvas, viewport }).promise;

      const url = canvas.toDataURL("image/jpeg", 0.82);
      pages.push(url.slice(url.indexOf(",") + 1));
      page.cleanup();
      onProgress?.(i, total);
      // 让出主线程，进度条才能实时刷新
      await new Promise((r) => setTimeout(r, 0));
    }
  } finally {
    // 渲染出错也要释放 worker 和内存
    await task.destroy();
    canvas.width = canvas.height = 0;
  }
  return pages;
}
