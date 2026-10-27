import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { Settings } from "./types";
import type { ThemeChoice } from "./theme";

type Props = {
  settings: Settings;
  theme: ThemeChoice;
  onTheme: (t: ThemeChoice) => void;
  onSaved: (s: Settings) => void;
  onClose: () => void;
};

type Test = { state: "idle" | "testing" | "ok" | "bad"; msg: string };

const THEMES: { id: ThemeChoice; label: string }[] = [
  { id: "system", label: "跟随系统" },
  { id: "light", label: "日间" },
  { id: "dark", label: "夜间" },
];

export default function SettingsDialog({ settings, theme, onTheme, onSaved, onClose }: Props) {
  const [key, setKey] = useState("");
  const [model, setModel] = useState(settings.model);
  const [test, setTest] = useState<Test>({ state: "idle", msg: "" });
  const [saving, setSaving] = useState(false);

  async function runTest() {
    setTest({ state: "testing", msg: "正在连接 DeepSeek…" });
    try {
      const models = await invoke<string[]>("test_connection", { apiKey: key || null });
      const m = model.trim() || "deepseek-flash";
      const msg =
        models.length === 0 || models.includes(m)
          ? "连接成功，Key 有效"
          : `Key 有效，但账户的模型列表里没有「${m}」。可用：${models.join("、")}`;
      setTest({ state: models.length === 0 || models.includes(m) ? "ok" : "bad", msg });
    } catch (e) {
      setTest({ state: "bad", msg: String(e) });
    }
  }

  async function save() {
    setSaving(true);
    try {
      const next = await invoke<Settings>("save_settings", {
        apiKey: key.trim() ? key.trim() : null,
        model,
      });
      onSaved(next);
      onClose();
    } catch (e) {
      setTest({ state: "bad", msg: String(e) });
      setSaving(false);
    }
  }

  return (
    <div className="overlay" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="job settings" onKeyDown={(e) => e.key === "Escape" && onClose()}>
        <div className="set-title">设置</div>

        <div className="field">
          <span className="field-l">外观</span>
          <div className="seg">
            {THEMES.map((t) => (
              <button key={t.id} className={theme === t.id ? "on" : ""} onClick={() => onTheme(t.id)}>
                {t.label}
              </button>
            ))}
          </div>
        </div>

        <label className="field">
          <span className="field-l">DeepSeek API Key</span>
          <input
            type="password"
            value={key}
            autoFocus
            spellCheck={false}
            placeholder={settings.has_key ? `已保存 ${settings.key_hint}，留空则不修改` : "sk-…"}
            onChange={(e) => {
              setKey(e.target.value);
              setTest({ state: "idle", msg: "" });
            }}
          />
          <span className="field-h">
            {settings.from_env
              ? "当前使用的是环境变量 DEEPSEEK_API_KEY，这里保存的 Key 只在没有环境变量时生效。"
              : "在 platform.deepseek.com 的 API Keys 页面创建。Key 只保存在本机，不会出现在界面代码里。"}
          </span>
        </label>

        <label className="field">
          <span className="field-l">模型</span>
          <input value={model} spellCheck={false} onChange={(e) => setModel(e.target.value)} />
          <span className="field-h">默认 deepseek-flash（支持读图）。deepseek-v4-pro 不支持图像，不能用。</span>
        </label>

        {test.msg && <div className={"test-msg " + test.state}>{test.msg}</div>}

        <div className="set-foot">
          <span className="field-h path" title={settings.data_dir}>
            数据位置：{settings.data_dir}
          </span>
          <button className="btn" onClick={runTest} disabled={test.state === "testing"}>
            测试连接
          </button>
          <button className="btn" onClick={onClose}>
            取消
          </button>
          <button className="btn primary" onClick={save} disabled={saving}>
            保存
          </button>
        </div>
      </div>
    </div>
  );
}
