import { useEffect, useState } from "react";

export type ThemeChoice = "system" | "light" | "dark";
export type Theme = "light" | "dark";

const KEY = "summa.theme";
const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

function load(): ThemeChoice {
  try {
    const v = localStorage.getItem(KEY);
    if (v === "light" || v === "dark" || v === "system") return v;
  } catch {}
  return "system";
}

function systemTheme(): Theme {
  return window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
}

/**
 * 主题：跟随系统 / 浅色 / 深色。
 * 选择存在本机；同时设置窗口外观，让红绿灯按钮和原生控件跟着变。
 */
export function useTheme() {
  const [choice, setChoice] = useState<ThemeChoice>(load);
  const [system, setSystem] = useState<Theme>(systemTheme);

  useEffect(() => {
    const mq = window.matchMedia("(prefers-color-scheme: light)");
    const on = () => setSystem(mq.matches ? "light" : "dark");
    mq.addEventListener("change", on);
    return () => mq.removeEventListener("change", on);
  }, []);

  const theme: Theme = choice === "system" ? system : choice;

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
  }, [theme]);

  useEffect(() => {
    try {
      localStorage.setItem(KEY, choice);
    } catch {}
    if (isTauri) {
      import("@tauri-apps/api/window")
        .then(({ getCurrentWindow }) => getCurrentWindow().setTheme(choice === "system" ? null : choice))
        .catch(() => {});
    }
  }, [choice]);

  return { theme, choice, setChoice };
}
