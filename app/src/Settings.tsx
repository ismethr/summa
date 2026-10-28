import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { Settings, SyncSettings } from "./types";
import type { ThemeChoice } from "./theme";

type Props = {
  settings: Settings;
  sync: SyncSettings | null;
  syncing: boolean;
  theme: ThemeChoice;
  onTheme: (t: ThemeChoice) => void;
  onSaved: (s: Settings) => void;
  onSyncSaved: (s: SyncSettings) => void;
  onSync: () => void;
  onClose: () => void;
};

type Test = { state: "idle" | "testing" | "ok" | "bad"; msg: string };

const THEMES: { id: ThemeChoice; label: string }[] = [
  { id: "system", label: "跟随系统" },
  { id: "light", label: "日间" },
  { id: "dark", label: "夜间" },
];

export default function SettingsDialog({
  settings,
  sync,
  syncing,
  theme,
  onTheme,
  onSaved,
  onSyncSaved,
  onSync,
  onClose,
}: Props) {
  const [key, setKey] = useState("");
  const [model, setModel] = useState(settings.model);
  const [test, setTest] = useState<Test>({ state: "idle", msg: "" });
  const [saving, setSaving] = useState(false);

  // WebDAV
  const [url, setUrl] = useState(sync?.url ?? "");
  const [user, setUser] = useState(sync?.username ?? "");
  const [pass, setPass] = useState("");
  const [insecure, setInsecure] = useState(sync?.insecure ?? false);
  const [enabled, setEnabled] = useState(sync?.enabled ?? false);
  const [auto, setAuto] = useState(sync?.auto ?? true);
  const [syncTest, setSyncTest] = useState<Test>({ state: "idle", msg: "" });

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

  /** 测试 WebDAV 前先把当前表单存下来，否则 Rust 侧用的是旧配置 */
  async function persistSync(): Promise<SyncSettings> {
    return invoke<SyncSettings>("save_sync_settings", {
      url,
      username: user,
      password: pass.trim() ? pass.trim() : null,
      insecure,
      enabled,
      auto,
    });
  }

  async function runSyncTest() {
    setSyncTest({ state: "testing", msg: "正在连接 WebDAV…" });
    try {
      const next = await persistSync();
      onSyncSaved(next);
      setPass("");
      const msg = await invoke<string>("test_sync");
      setSyncTest({ state: "ok", msg });
    } catch (e) {
      setSyncTest({ state: "bad", msg: String(e) });
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
      const nextSync = await persistSync();
      onSyncSaved(nextSync);
      // 用户可能是刚填完地址就点保存，顺手同步一次，省一步操作
      if (nextSync.enabled && !sync?.enabled) onSync();
      onClose();
    } catch (e) {
      setSyncTest({ state: "bad", msg: String(e) });
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

        <div className="set-sec">同步（WebDAV）</div>

        <label className="field">
          <span className="field-l">服务器地址</span>
          <input
            value={url}
            spellCheck={false}
            placeholder="https://nas.example.com:5006/homes"
            onChange={(e) => {
              setUrl(e.target.value);
              setSyncTest({ state: "idle", msg: "" });
            }}
          />
          <span className="field-h">
            填到共享文件夹一级，不要带末尾斜杠。应用会在其下建 summa 目录。群晖的 WebDAV 端口通常是 HTTPS 5006。
          </span>
        </label>

        <div className="field-row">
          <label className="field">
            <span className="field-l">用户名</span>
            <input value={user} spellCheck={false} autoComplete="off" onChange={(e) => setUser(e.target.value)} />
          </label>
          <label className="field">
            <span className="field-l">密码</span>
            <input
              type="password"
              value={pass}
              autoComplete="new-password"
              placeholder={sync?.has_password ? `已保存 ${sync.password_hint}` : ""}
              onChange={(e) => {
                setPass(e.target.value);
                setSyncTest({ state: "idle", msg: "" });
              }}
            />
          </label>
        </div>

        <label className="check">
          <input type="checkbox" checked={insecure} onChange={(e) => setInsecure(e.target.checked)} />
          接受自签名证书
          <span className="check-h">群晖默认证书是自签名的，不勾选会连不上</span>
        </label>

        <label className="check">
          <input type="checkbox" checked={enabled} onChange={(e) => setEnabled(e.target.checked)} />
          启用同步
        </label>

        <label className="check">
          <input
            type="checkbox"
            checked={auto}
            disabled={!enabled}
            onChange={(e) => setAuto(e.target.checked)}
          />
          打开应用时自动同步
        </label>

        <div className="set-note">
          同步到服务器的是题录、要义、主题、笔记和 PDF 原文。同一篇在两台机器上都改过时，以修改时间晚的一台为准；
          在一台删除的文献不会被另一台带回来。
        </div>

        <div className="sync-test-row">
          <button className="btn" onClick={runSyncTest} disabled={syncTest.state === "testing" || !url.trim()}>
            测试同步
          </button>
          <span className="field-h">只建目录并读一次远端库，不改动任何数据</span>
        </div>

        {syncTest.msg && <div className={"test-msg " + syncTest.state}>{syncTest.msg}</div>}

        <div className="set-foot">
          <span className="field-h path" title={settings.data_dir}>
            {settings.data_dir}
          </span>
          <button className="btn" onClick={runTest} disabled={test.state === "testing"}>
            测试 Key
          </button>
          <button className="btn" onClick={onClose}>
            取消
          </button>
          <button className="btn primary" onClick={save} disabled={saving}>
            保存{syncing ? "（同步中）" : ""}
          </button>
        </div>
      </div>
    </div>
  );
}
