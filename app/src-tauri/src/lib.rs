use serde::{Deserialize, Deserializer, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::Manager;

mod sync;

const MAX_PAGES: usize = 12;
/// DeepSeek 官方说明：deepseek-flash 即 V4.1 Flash，支持图像输入；v4-pro 不支持图像
const DEFAULT_MODEL: &str = "deepseek-flash";
const API_BASE: &str = "https://api.deepseek.com";
const MAX_PDF_BYTES: u64 = 80 * 1024 * 1024;
/// 墓碑上限。每条只有几十字节，留一千条足够覆盖多端来回同步的窗口期。
const MAX_TOMBSTONES: usize = 1000;

/// 文献库读写锁。连续拖入多篇时，几次 parse_pdf 会同时"读-改-写" library.json，
/// 不加锁的话后写的会把先写的冲掉。
static LIBRARY_LOCK: Mutex<()> = Mutex::new(());

// ---------- 宽容反序列化 ----------
//
// prompt 要求模型"看不清就填 null"，但 String 字段默认不接受 null，
// 一条 null 就会让整篇解析报"字段结构不符"。下面几个函数把 null、数字、
// 数组等常见偏差都转成合理的值。

fn lenient_string<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    use serde_json::Value;
    Ok(match Value::deserialize(d)? {
        Value::Null => String::new(),
        Value::String(s) => s,
        Value::Array(arr) => arr
            .iter()
            .map(|v| match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .collect::<Vec<_>>()
            .join(", "),
        other => other.to_string(),
    })
}

fn lenient_year<'de, D: Deserializer<'de>>(d: D) -> Result<i32, D::Error> {
    use serde_json::Value;
    Ok(match Value::deserialize(d)? {
        Value::Number(n) => n.as_i64().unwrap_or(0) as i32,
        Value::String(s) => s
            .chars()
            .filter(|c| c.is_ascii_digit())
            .take(4)
            .collect::<String>()
            .parse()
            .unwrap_or(0),
        _ => 0,
    })
}

fn lenient_vec<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(d)?.unwrap_or_default())
}

fn lenient_tags<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    use serde_json::Value;
    Ok(match Value::deserialize(d)? {
        Value::Array(arr) => arr
            .into_iter()
            .filter_map(|v| match v {
                Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
                _ => None,
            })
            .collect(),
        Value::String(s) => s
            .split([',', '，', '、'])
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect(),
        _ => Vec::new(),
    })
}

/// 按字符截断。直接 `&s[..n]` 会切在中文字符中间导致程序崩溃。
fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

// ---------- 数据结构 ----------

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Step {
    #[serde(default, deserialize_with = "lenient_string")]
    pub name: String,
    #[serde(default, deserialize_with = "lenient_string")]
    pub detail: String,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Finding {
    #[serde(default, deserialize_with = "lenient_string")]
    pub value: String,
    #[serde(default, deserialize_with = "lenient_string")]
    pub label: String,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Paper {
    #[serde(default, deserialize_with = "lenient_string")]
    pub id: String,
    #[serde(default, deserialize_with = "lenient_string")]
    pub title: String,
    #[serde(default, deserialize_with = "lenient_string")]
    pub authors: String,
    #[serde(default, deserialize_with = "lenient_string")]
    pub venue: String,
    #[serde(default, deserialize_with = "lenient_year")]
    pub year: i32,
    #[serde(default, deserialize_with = "lenient_tags")]
    pub tags: Vec<String>,
    #[serde(default, deserialize_with = "lenient_string")]
    pub gist: String,
    #[serde(default, deserialize_with = "lenient_string")]
    pub problem: String,
    #[serde(default, deserialize_with = "lenient_vec")]
    pub steps: Vec<Step>,
    #[serde(default, deserialize_with = "lenient_vec")]
    pub findings: Vec<Finding>,
    #[serde(default, deserialize_with = "lenient_string")]
    pub limits: String,
    /// 库内保存的 PDF 副本路径（app_data_dir/files/<id>.pdf）
    #[serde(default)]
    pub file: String,
    /// 拖入时的原始路径，用来识别"同一个文件再拖一次"
    #[serde(default)]
    pub source: String,
    /// 入库时间（毫秒时间戳）
    #[serde(default)]
    pub added: u64,
    /// 已读
    #[serde(default)]
    pub read: bool,
    /// 星标
    #[serde(default)]
    pub starred: bool,
    /// 用户自己的笔记
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub doi: String,
    /// 记录来自哪里：空 = 拖入解析；workbuddy / claude-code 等 = 外部工具导入
    #[serde(default)]
    pub origin: String,
    /// 修订时间（毫秒）。同步时同一篇以 rev 更大的一侧为准。
    /// 老数据没有这个字段，合并时用 `added` 兜底，见 `sheet_rev`。
    #[serde(default)]
    pub rev: u64,
}

/// 界面上可以手动修改的字段；None 表示不改
#[derive(Deserialize)]
pub struct PaperPatch {
    tags: Option<Vec<String>>,
    read: Option<bool>,
    starred: Option<bool>,
    note: Option<String>,
}

/// 删除标记。多端同步时，一台机器删掉的文献如果不留标记，
/// 另一台机器下次同步会把它当成「只存在于远端」再传回来，也就是复活。
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Tombstone {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub at: u64,
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub(crate) struct Library {
    #[serde(default)]
    pub(crate) papers: Vec<Paper>,
    /// 已删除文献的墓碑，同步时取并集；超过上限会自动裁剪
    #[serde(default)]
    pub(crate) tombstones: Vec<Tombstone>,
}

// ---------- 工具 ----------

fn data_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("无法获取应用数据目录：{e}"))?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建目录失败：{e}"))?;
    Ok(dir)
}

fn library_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(data_dir(app)?.join("library.json"))
}

fn read_library(app: &tauri::AppHandle) -> Result<Library, String> {
    let file = library_path(app)?;
    if !file.exists() {
        return Ok(Library::default());
    }
    let raw = std::fs::read_to_string(&file).map_err(|e| format!("读取文献库失败：{e}"))?;
    serde_json::from_str(&raw).map_err(|e| format!("文献库格式有误：{e}"))
}

fn write_library(app: &tauri::AppHandle, lib: &Library) -> Result<(), String> {
    let file = library_path(app)?;
    let text = serde_json::to_string_pretty(lib).map_err(|e| format!("序列化失败：{e}"))?;
    // 先写临时文件再改名：写到一半断电或崩溃时，原来的 library.json 不会被写坏
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("写入失败：{e}"))?;
    std::fs::rename(&tmp, &file).map_err(|e| format!("写入失败：{e}"))
}

fn now_stamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 合并同一篇文献时的先后依据。老数据没有 rev，用入库时间兜底；
/// 两者都是毫秒时间戳，可以直接比大小。
fn sheet_rev(p: &Paper) -> u64 {
    p.rev.max(p.added)
}

/// 逐篇对比 old 与 new，内容有变化的刷新 rev。
///
/// rev 只在这里写，是为了让"同步判新旧"这件事只有一个入口：
/// 各个命令各自记得更新 rev 的话，早晚会漏掉一处，而漏掉的表现是
/// 「这台机器上的修改在另一台机器上被旧数据覆盖」，很难查。
fn bump_changed(old: &Library, new: &mut Library) {
    use std::collections::HashMap;
    let prev: HashMap<&str, &Paper> = old.papers.iter().map(|p| (p.id.as_str(), p)).collect();
    let now = now_stamp();
    for p in new.papers.iter_mut() {
        let unchanged = prev
            .get(p.id.as_str())
            .map(|o| {
                serde_json::to_string(o).ok().zip(serde_json::to_string(p).ok()).is_some_and(|(a, b)| a == b)
            })
            .unwrap_or(false);
        if !unchanged {
            p.rev = now;
        }
    }
}

/// 所有写库操作都走这里：先刷 rev，再落盘。
fn commit_library(app: &tauri::AppHandle, old: &Library, new: &mut Library) -> Result<(), String> {
    bump_changed(old, new);
    write_library(app, new)
}

// ---------- 设置 ----------
//
// 配置存在 app_data_dir/config.json：{ "api_key": "...", "model": "..." }。
// 必须支持配置文件：macOS 上双击启动的 .app 不继承 shell 环境变量，
// 只靠 env var 的话打包后一定读不到。

fn config_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(data_dir(app)?.join("config.json"))
}

fn read_config(app: &tauri::AppHandle) -> serde_json::Map<String, serde_json::Value> {
    config_path(app)
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn env_key() -> Option<String> {
    ["DEEPSEEK_API_KEY", "SUMMA_API_KEY"].iter().find_map(|name| {
        let v = std::env::var(name).ok()?.trim().to_string();
        (!v.is_empty()).then_some(v)
    })
}

/// 读 API Key。顺序：环境变量 → config.json。
fn api_key(app: &tauri::AppHandle) -> Option<String> {
    env_key().or_else(|| {
        let key = read_config(app).get("api_key")?.as_str()?.trim().to_string();
        (!key.is_empty()).then_some(key)
    })
}

fn model_name(app: &tauri::AppHandle) -> String {
    read_config(app)
        .get("model")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string())
}

/// 只给界面看首尾几位，完整 Key 永远不回传前端
fn mask_key(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 10 {
        return "•".repeat(chars.len());
    }
    let head: String = chars[..5].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}••••{tail}")
}

fn http_client(timeout_secs: u64) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(timeout_secs))
        .build()
        .map_err(|e| format!("创建网络客户端失败：{e}"))
}

fn friendly_status(status: reqwest::StatusCode, body: &str) -> String {
    let hint = match status.as_u16() {
        401 => "API Key 无效，请在设置里检查是否填错",
        402 => "账户余额不足，请到 DeepSeek 平台充值",
        429 => "请求太频繁，稍等片刻再试",
        500..=599 => "DeepSeek 服务暂时异常，稍后再试",
        _ => "",
    };
    if hint.is_empty() {
        format!("接口返回 {status}：{}", clip(body, 400))
    } else {
        format!("{hint}（{status}）")
    }
}

fn request_error(e: reqwest::Error) -> String {
    if e.is_timeout() {
        "请求超时：模型没有及时返回，请检查网络后重试".to_string()
    } else if e.is_connect() {
        "连不上 DeepSeek，请检查网络".to_string()
    } else {
        format!("请求失败：{e}")
    }
}

/// 从模型回复里抠出 JSON。模型常会包 ```json 代码块或加一句前言，
/// 所以取第一个 { 到最后一个 } 之间的内容，比直接 parse 稳。
fn extract_json(text: &str) -> Option<serde_json::Value> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str(&text[start..=end]).ok()
}

// ---------- 命令 ----------

#[tauri::command]
fn load_library(app: tauri::AppHandle) -> Result<Library, String> {
    let _guard = LIBRARY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    read_library(&app)
}

/// 读取 PDF 原始字节，交给前端用 PDF.js 渲染。
/// 走 Rust 而不是前端插件，是为了拿到真实路径、并统一做大小限制。
///
/// 用 `ipc::Response` 直接传二进制，前端拿到的是 ArrayBuffer。
/// 比转 base64 少了编码/解码两步，体积也小 1/3，大文件明显更快。
#[tauri::command]
fn read_pdf(path: String) -> Result<tauri::ipc::Response, String> {
    let meta = std::fs::metadata(&path).map_err(|e| format!("无法读取文件：{e}"))?;
    if meta.len() > MAX_PDF_BYTES {
        return Err("文件超过 80 MB，暂不支持".into());
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("读取失败：{e}"))?;
    Ok(tauri::ipc::Response::new(bytes))
}

/// 解析一篇论文：pages 是前端渲染好的页面图像（JPEG base64）。
/// 成功后把 PDF 复制进库里，原文件被移动或删除也不影响打开。
#[tauri::command]
async fn parse_pdf(
    app: tauri::AppHandle,
    path: String,
    file_name: String,
    pages: Vec<String>,
) -> Result<Paper, String> {
    if pages.is_empty() {
        return Err("没有可解析的页面图像".into());
    }
    let key = api_key(&app).ok_or("NO_KEY")?;
    let used = pages.len().min(MAX_PAGES);
    let stem = match file_name.len().checked_sub(4) {
        Some(i) if file_name.is_char_boundary(i) && file_name[i..].eq_ignore_ascii_case(".pdf") => {
            file_name[..i].to_string()
        }
        _ => file_name.clone(),
    };

    let mut paper = call_model(&key, &model_name(&app), &stem, &pages[..used]).await?;

    let _guard = LIBRARY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut lib = read_library(&app)?;
    let old = lib.clone();

    // 同一个文件再拖一次：沿用原来的 id，覆盖旧记录
    let existing = lib.papers.iter().position(|p| !p.source.is_empty() && p.source == path);
    paper.id = match existing {
        Some(i) => lib.papers[i].id.clone(),
        None => format!("p{}", now_stamp()),
    };
    paper.source = path.clone();
    paper.added = now_stamp();
    // 重新解析时保留用户自己标的状态和笔记
    if let Some(i) = existing {
        let old = &lib.papers[i];
        paper.read = old.read;
        paper.starred = old.starred;
        paper.note = old.note.clone();
    }

    let files = data_dir(&app)?.join("files");
    std::fs::create_dir_all(&files).map_err(|e| format!("创建目录失败：{e}"))?;
    let copy = files.join(format!("{}.pdf", paper.id));
    std::fs::copy(&path, &copy).map_err(|e| format!("保存 PDF 副本失败：{e}"))?;
    paper.file = copy.to_string_lossy().into_owned();

    if let Some(i) = existing {
        lib.papers.remove(i);
    }
    // 这篇之前可能被删过，重新拖进来就撤掉墓碑，否则下次同步又会被删掉
    lib.tombstones.retain(|t| t.id != paper.id);
    lib.papers.insert(0, paper.clone());
    commit_library(&app, &old, &mut lib)?;

    Ok(paper)
}

/// 删除一篇或多篇：同时删掉库里的 PDF 副本（不碰用户的原文件）
#[tauri::command]
fn delete_papers(app: tauri::AppHandle, ids: Vec<String>) -> Result<(), String> {
    let _guard = LIBRARY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut lib = read_library(&app)?;
    let old = lib.clone();
    let (removed, kept): (Vec<Paper>, Vec<Paper>) =
        lib.papers.into_iter().partition(|p| ids.contains(&p.id));
    lib.papers = kept;
    // 留墓碑：否则另一台机器下次同步会把这些文献当成"远端独有"再传回来
    let now = now_stamp();
    for p in &removed {
        lib.tombstones.retain(|t| t.id != p.id);
        lib.tombstones.push(Tombstone { id: p.id.clone(), at: now });
    }
    if lib.tombstones.len() > MAX_TOMBSTONES {
        lib.tombstones.sort_by(|a, b| b.at.cmp(&a.at));
        lib.tombstones.truncate(MAX_TOMBSTONES);
    }
    commit_library(&app, &old, &mut lib)?;
    let files = data_dir(&app)?.join("files");
    for p in removed {
        let file = PathBuf::from(&p.file);
        if file.starts_with(&files) {
            let _ = std::fs::remove_file(file);
        }
    }
    Ok(())
}

/// 修改一篇的标签、已读、星标、笔记
#[tauri::command]
fn update_paper(app: tauri::AppHandle, id: String, patch: PaperPatch) -> Result<Paper, String> {
    let _guard = LIBRARY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut lib = read_library(&app)?;
    let old = lib.clone();
    let paper = lib.papers.iter_mut().find(|p| p.id == id).ok_or("找不到这篇文献")?;
    if let Some(tags) = patch.tags {
        let mut clean: Vec<String> = Vec::new();
        for t in tags.iter().map(|t| t.trim()).filter(|t| !t.is_empty()) {
            if !clean.iter().any(|c| c == t) {
                clean.push(t.to_string());
            }
        }
        paper.tags = clean;
    }
    if let Some(v) = patch.read {
        paper.read = v;
    }
    if let Some(v) = patch.starred {
        paper.starred = v;
    }
    if let Some(v) = patch.note {
        paper.note = v;
    }
    let updated = paper.clone();
    commit_library(&app, &old, &mut lib)?;
    Ok(updated)
}

/// 全库重命名一个主题；新名字已存在时等于合并两个主题；新名字为空表示删除这个主题
fn retag(lib: &mut Library, from: &str, to: &str) {
    for p in lib.papers.iter_mut() {
        if p.tags.iter().any(|t| t == from) {
            let mut next: Vec<String> = Vec::new();
            for t in p.tags.drain(..) {
                let t = if t == from { to.to_string() } else { t };
                if !t.is_empty() && !next.contains(&t) {
                    next.push(t);
                }
            }
            p.tags = next;
        }
    }
}

#[tauri::command]
fn rename_tag(app: tauri::AppHandle, from: String, to: String) -> Result<Library, String> {
    let to = to.trim().to_string();
    if to.is_empty() {
        return Err("主题名不能为空".into());
    }
    let _guard = LIBRARY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut lib = read_library(&app)?;
    let old = lib.clone();
    retag(&mut lib, &from, &to);
    commit_library(&app, &old, &mut lib)?;
    Ok(lib)
}

/// 从所有文献上移除一个主题（文献本身不删）
#[tauri::command]
fn delete_tag(app: tauri::AppHandle, name: String) -> Result<Library, String> {
    let _guard = LIBRARY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut lib = read_library(&app)?;
    let old = lib.clone();
    retag(&mut lib, &name, "");
    commit_library(&app, &old, &mut lib)?;
    Ok(lib)
}

// ---------- 外部工具导入（收件箱） ----------
//
// WorkBuddy / Claude Code 等工具把论文 JSON 写进 app_data_dir/inbox/，
// 应用启动时和运行中每隔几秒调用 import_inbox 收进文献库。
// 这样 library.json 始终只有应用自己在写，外部工具不会和界面抢着写坏文件。

/// 去重用的标题键：小写，只留字母数字和汉字
fn title_key(title: &str) -> String {
    title
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

fn norm_doi(doi: &str) -> String {
    let d = doi.trim().to_lowercase();
    let d = d
        .trim_start_matches("https://doi.org/")
        .trim_start_matches("http://doi.org/")
        .trim_start_matches("https://dx.doi.org/")
        .trim_start_matches("doi:")
        .trim();
    d.to_string()
}

/// 取文本：既接受 "xxx"，也接受 {"text": "xxx", "page": 3}
fn text_of(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.trim().to_string(),
        serde_json::Value::Object(o) => o.get("text").map(text_of).unwrap_or_default(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// 把外部工具写的 JSON 转成 Paper。字段名兼容 skill 文档里的写法和 Summa 自己的写法。
fn paper_from_external(v: &serde_json::Value) -> Result<(Paper, String), String> {
    use serde_json::Value;
    let title = text_of(&v["title"]);
    if title.is_empty() {
        return Err("缺少 title".into());
    }
    let authors = match &v["authors"] {
        Value::Array(a) => a.iter().map(text_of).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", "),
        other => text_of(other),
    };
    let year = match &v["year"] {
        Value::Number(n) => n.as_i64().unwrap_or(0) as i32,
        Value::String(s) => s.chars().filter(|c| c.is_ascii_digit()).take(4).collect::<String>().parse().unwrap_or(0),
        _ => 0,
    };
    let tag_src = if v["topics"].is_null() { &v["tags"] } else { &v["topics"] };
    let tags = match tag_src {
        Value::Array(a) => a
            .iter()
            .map(|t| if t.is_object() { text_of(&t["name"]) } else { text_of(t) })
            .filter(|s| !s.is_empty())
            .collect(),
        Value::String(s) => s.split([',', '，', '、']).map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect(),
        _ => Vec::new(),
    };
    let step_src = if v["method_steps"].is_null() { &v["steps"] } else { &v["method_steps"] };
    let steps = step_src
        .as_array()
        .map(|a| {
            a.iter()
                .map(|s| Step { name: text_of(&s["name"]), detail: text_of(&s["detail"]) })
                .filter(|s| !s.name.is_empty() || !s.detail.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let findings = v["findings"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|f| {
                    // skill 写法是 metric + value + dataset，界面用 value + label
                    let label = if f["label"].is_null() {
                        [text_of(&f["metric"]), text_of(&f["dataset"])]
                            .into_iter()
                            .filter(|s| !s.is_empty())
                            .collect::<Vec<_>>()
                            .join(" · ")
                    } else {
                        text_of(&f["label"])
                    };
                    Finding { value: text_of(&f["value"]), label }
                })
                .filter(|f| !f.value.is_empty() || !f.label.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let paper = Paper {
        title,
        authors,
        venue: text_of(&v["venue"]),
        year,
        tags,
        gist: text_of(&v["gist"]),
        problem: text_of(&v["problem"]),
        steps,
        findings,
        limits: text_of(&v["limits"]),
        doi: norm_doi(&text_of(&v["doi"])),
        origin: {
            let s = text_of(&v["source"]);
            if s.is_empty() { "外部导入".into() } else { s }
        },
        ..Default::default()
    };
    Ok((paper, text_of(&v["pdf_path"])))
}

/// 合并到已有记录：新内容非空才覆盖，主题取并集；用户自己的已读、星标、笔记、PDF 不动
fn merge_into(old: &mut Paper, new: Paper) {
    fn take(old: &mut String, new: String) {
        if !new.trim().is_empty() {
            *old = new;
        }
    }
    take(&mut old.title, new.title);
    take(&mut old.authors, new.authors);
    take(&mut old.venue, new.venue);
    take(&mut old.gist, new.gist);
    take(&mut old.problem, new.problem);
    take(&mut old.limits, new.limits);
    take(&mut old.doi, new.doi);
    if new.year != 0 {
        old.year = new.year;
    }
    if !new.steps.is_empty() {
        old.steps = new.steps;
    }
    if !new.findings.is_empty() {
        old.findings = new.findings;
    }
    for t in new.tags {
        if !old.tags.contains(&t) {
            old.tags.push(t);
        }
    }
}

#[derive(Serialize, Default)]
struct ImportResult {
    created: Vec<String>,
    updated: Vec<String>,
    failed: Vec<String>,
    /// 有变化时返回整个库，界面直接替换
    library: Option<Library>,
}

#[tauri::command]
fn import_inbox(app: tauri::AppHandle) -> Result<ImportResult, String> {
    let inbox = data_dir(&app)?.join("inbox");
    std::fs::create_dir_all(&inbox).map_err(|e| format!("创建收件箱失败：{e}"))?;
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&inbox)
        .map_err(|e| format!("读取收件箱失败：{e}"))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "json"))
        .collect();
    let mut result = ImportResult::default();
    if entries.is_empty() {
        return Ok(result);
    }
    entries.sort();

    let _guard = LIBRARY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut lib = read_library(&app)?;
    let old = lib.clone();
    let files_dir = data_dir(&app)?.join("files");
    let failed_dir = inbox.join("failed");

    for path in entries {
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let parsed = std::fs::read_to_string(&path)
            .map_err(|e| format!("读取失败：{e}"))
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).map_err(|e| format!("不是合法 JSON：{e}")));
        // 支持单篇对象、数组、{"papers": [...]} 三种写法
        let items: Vec<serde_json::Value> = match parsed {
            Ok(serde_json::Value::Array(a)) => a,
            Ok(v) if v["papers"].is_array() => v["papers"].as_array().cloned().unwrap_or_default(),
            Ok(v) => vec![v],
            Err(e) => {
                let _ = std::fs::create_dir_all(&failed_dir);
                let _ = std::fs::write(failed_dir.join(format!("{name}.error.txt")), &e);
                let _ = std::fs::rename(&path, failed_dir.join(&name));
                result.failed.push(format!("{name}：{e}"));
                continue;
            }
        };

        let mut errors = Vec::new();
        for item in items {
            let (paper, pdf) = match paper_from_external(&item) {
                Ok(x) => x,
                Err(e) => {
                    errors.push(e);
                    continue;
                }
            };
            let key = title_key(&paper.title);
            let existing = lib.papers.iter().position(|p| {
                (!paper.doi.is_empty() && norm_doi(&p.doi) == paper.doi) || title_key(&p.title) == key
            });
            let title = paper.title.clone();
            let idx = match existing {
                Some(i) => {
                    merge_into(&mut lib.papers[i], paper);
                    result.updated.push(title);
                    i
                }
                None => {
                    let mut paper = paper;
                    paper.id = format!("p{}{}", now_stamp(), lib.papers.len());
                    paper.added = now_stamp();
                    if paper.tags.is_empty() {
                        paper.tags = vec!["待归类".into()];
                    }
                    lib.papers.insert(0, paper);
                    result.created.push(title);
                    0
                }
            };
            // 附带了本地 PDF 路径且库里还没有副本时，复制一份进来
            let p = &mut lib.papers[idx];
            if p.file.is_empty() && !pdf.is_empty() && std::path::Path::new(&pdf).is_file() {
                let _ = std::fs::create_dir_all(&files_dir);
                let copy = files_dir.join(format!("{}.pdf", p.id));
                if std::fs::copy(&pdf, &copy).is_ok() {
                    p.file = copy.to_string_lossy().into_owned();
                    p.source = pdf;
                }
            }
        }

        if errors.is_empty() {
            let _ = std::fs::remove_file(&path);
        } else {
            let _ = std::fs::create_dir_all(&failed_dir);
            let _ = std::fs::write(failed_dir.join(format!("{name}.error.txt")), errors.join("\n"));
            let _ = std::fs::rename(&path, failed_dir.join(&name));
            result.failed.push(format!("{name}：{}", errors.join("；")));
        }
    }

    if !result.created.is_empty() || !result.updated.is_empty() {
        commit_library(&app, &old, &mut lib)?;
        result.library = Some(lib);
    }
    Ok(result)
}

/// 用系统默认程序（预览、Acrobat 等）打开 PDF。
/// 只接受 id，不接受任意路径，避免界面层能打开库外的文件。
#[tauri::command]
fn open_paper(app: tauri::AppHandle, id: String) -> Result<(), String> {
    let lib = {
        let _guard = LIBRARY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        read_library(&app)?
    };
    let paper = lib.papers.iter().find(|p| p.id == id).ok_or("找不到这篇文献")?;
    let target = [&paper.file, &paper.source]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| !p.as_os_str().is_empty() && p.exists())
        .ok_or("找不到 PDF 文件，可能已被移动或删除。重新拖入一次即可。")?;

    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = std::process::Command::new("xdg-open");

    cmd.arg(target).spawn().map_err(|e| format!("打开失败：{e}"))?;
    Ok(())
}

#[derive(Serialize)]
struct Settings {
    /// 是否已配置 Key
    has_key: bool,
    /// 打码后的 Key，如 sk-ab••••wxyz
    key_hint: String,
    /// Key 来自环境变量时，界面里改不了，要提示用户
    from_env: bool,
    model: String,
    data_dir: String,
}

#[tauri::command]
fn get_settings(app: tauri::AppHandle) -> Result<Settings, String> {
    let key = api_key(&app);
    Ok(Settings {
        has_key: key.is_some(),
        key_hint: key.as_deref().map(mask_key).unwrap_or_default(),
        from_env: env_key().is_some(),
        model: model_name(&app),
        data_dir: data_dir(&app)?.to_string_lossy().into_owned(),
    })
}

/// 保存设置。api_key 为 None 表示不改；为空字符串表示清除。
#[tauri::command]
fn save_settings(
    app: tauri::AppHandle,
    api_key: Option<String>,
    model: String,
) -> Result<Settings, String> {
    let mut cfg = read_config(&app);
    if let Some(k) = api_key {
        cfg.insert("api_key".into(), k.trim().into());
    }
    cfg.insert("model".into(), model.trim().into());
    let text = serde_json::to_string_pretty(&cfg).map_err(|e| format!("序列化失败：{e}"))?;
    std::fs::write(config_path(&app)?, text).map_err(|e| format!("保存设置失败：{e}"))?;
    get_settings(app)
}

/// 测试连接：用 Key 请求模型列表，能通说明 Key 有效、网络可达。
/// api_key 为 None 时测已保存的 Key。
#[tauri::command]
async fn test_connection(app: tauri::AppHandle, api_key: Option<String>) -> Result<Vec<String>, String> {
    let key = api_key
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .or_else(|| self::api_key(&app))
        .ok_or("还没有填写 API Key")?;
    let resp = http_client(20)?
        .get(format!("{API_BASE}/models"))
        .bearer_auth(key)
        .send()
        .await
        .map_err(request_error)?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| format!("读取响应失败：{e}"))?;
    if !status.is_success() {
        return Err(friendly_status(status, &text));
    }
    let json: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
    Ok(json["data"]
        .as_array()
        .map(|arr| arr.iter().filter_map(|m| m["id"].as_str().map(String::from)).collect())
        .unwrap_or_default())
}

// ---------- 模型调用 ----------

async fn call_model(key: &str, model: &str, stem: &str, pages: &[String]) -> Result<Paper, String> {
    let mut content = vec![serde_json::json!({
        "type": "text",
        "text": format!(
            "这是一篇论文《{stem}》的页面图像，共 {} 页。请抽取信息并只输出 JSON。\n\n\
             输出格式：\n\
             {{\"title\":\"原文标题\",\"authors\":\"Chen, L., Wang, Y.\",\"venue\":\"期刊或会议名\",\
             \"year\":2025,\"tags\":[\"主题词1\",\"主题词2\"],\"gist\":\"一句话要旨，不超过 60 字\",\
             \"problem\":\"作者要解决的问题\",\"steps\":[{{\"name\":\"步骤名\",\"detail\":\"做了什么\"}}],\
             \"findings\":[{{\"value\":\"+12.4%\",\"label\":\"这个数字说明什么\"}}],\
             \"limits\":\"作者自述的局限\"}}\n\n\
             规则：\n\
             1. 所有数值逐字照抄原文，看不清或没有就填 null，禁止推测和单位换算。\n\
             2. authors 用字符串，姓在前、名缩写在后，如 \"Chen, L., Wang, Y.\"；\n\
                不要用「名 姓」顺序，不要数组。\n\
             3. steps 给 3 步，每步 name 是短动宾短语。\n\
             4. findings 最多 3 条，只取正文用文字强调的结果，不要图表里出现但作者没主张的数字。\n\
                value 只放数值本身（含单位或百分比），不超过 12 个字符，如 \"+12.4%\"、\"128.9 万 ha\"；\n\
                这个数字说明什么，写在 label 里，不要塞进 value。\n\
             5. limits 只写作者自己承认的局限，不要用你的领域知识补充；作者没提就填空字符串。\n\
             6. 全部用中文表述，方法名、模型名、指标名等专有名词保留英文原文，\n\
                不要出现「耕地扩张 preferential 占用」这种半英半中的写法。\n\
             7. tags 给 1–3 个主题词，用中文；不要放国名、地区名、数据源名这类范围词。\n\
             8. 只输出 JSON，不要任何解释文字。",
            pages.len()
        )
    })];

    for p in pages {
        content.push(serde_json::json!({
            "type": "image_url",
            "image_url": { "url": format!("data:image/jpeg;base64,{p}") }
        }));
    }

    let body = serde_json::json!({
        "model": model,
        "temperature": 0.1,
        "messages": [
            { "role": "system", "content": "你是遥感与地理信息领域的论文审读者，只输出 JSON。" },
            { "role": "user", "content": content }
        ]
    });

    // 不设超时的话，网络卡住时界面会永远停在"模型解析中"
    let resp = http_client(300)?
        .post(format!("{API_BASE}/chat/completions"))
        .bearer_auth(key)
        .json(&body)
        .send()
        .await
        .map_err(request_error)?;

    let status = resp.status();
    let text = resp.text().await.map_err(|e| format!("读取响应失败：{e}"))?;

    if !status.is_success() {
        return Err(friendly_status(status, &text));
    }

    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("响应不是 JSON：{e}"))?;

    let raw = json["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.to_string())
        .or_else(|| {
            json["choices"][0]["message"]["content"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|p| p["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("")
                })
        })
        .ok_or_else(|| "响应里没有 content".to_string())?;

    let parsed = extract_json(&raw)
        .ok_or_else(|| format!("模型没有返回可解析的 JSON，原始内容：{}", clip(&raw, 300)))?;

    let mut paper: Paper = serde_json::from_value(parsed)
        .map_err(|e| format!("字段结构不符：{e}"))?;

    if paper.title.trim().is_empty() {
        paper.title = stem.to_string();
    }
    paper.id = format!("p{}", now_stamp());
    if paper.tags.is_empty() {
        paper.tags = vec!["待归类".into()];
    }
    normalize_findings(&mut paper);
    Ok(paper)
}

/// 数值卡片的保底整形。
///
/// fresh 数据里见过 value 塞进整句（「128.9 × 10^4 ha，年均 4.03 × 10^4 ha」），
/// 界面上这个字段是 19px 大字，一句话就撑破整行。prompt 已经约束，
/// 但模型不总听话，这里再兜一层：超过 3 条的截断，过长的 value 挪进 label。
fn normalize_findings(paper: &mut Paper) {
    paper.findings.truncate(3);
    for f in paper.findings.iter_mut() {
        if f.value.chars().count() <= 24 {
            continue;
        }
        // 取到第一个数字段落（遇到逗号、分号、句号就停）
        let head: String = f
            .value
            .chars()
            .take_while(|c| !matches!(c, '，' | ',' | '；' | ';' | '。'))
            .take(24)
            .collect();
        if f.label.trim().is_empty() {
            f.label = f.value.clone();
        }
        f.value = head.trim().to_string();
    }
    // 标签太多说明模型把范围词也当主题了
    paper.tags.truncate(3);
}

// ---------- 启动 ----------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            load_library,
            read_pdf,
            parse_pdf,
            delete_papers,
            delete_tag,
            import_inbox,
            update_paper,
            rename_tag,
            open_paper,
            get_settings,
            save_settings,
            test_connection,
            sync::get_sync_settings,
            sync::save_sync_settings,
            sync::test_sync,
            sync::sync_now
        ])
        .run(tauri::generate_context!())
        .expect("Summa 启动失败");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 模型返回null和偏差格式也能解析() {
        let raw = r#"{"title":"T","authors":["Chen, L.","Wang, Y."],"venue":null,
            "year":"2025年","tags":"遥感、变化检测","gist":null,"problem":"P",
            "steps":null,"findings":[{"value":null,"label":"mIoU"},{"value":12.4,"label":"x"}],
            "limits":null}"#;
        let p: Paper = serde_json::from_value(extract_json(raw).unwrap()).unwrap();
        assert_eq!(p.authors, "Chen, L., Wang, Y.");
        assert_eq!(p.year, 2025);
        assert_eq!(p.tags, vec!["遥感", "变化检测"]);
        assert!(p.steps.is_empty());
        assert_eq!(p.findings[0].value, "");
        assert_eq!(p.findings[1].value, "12.4");
    }

    #[test]
    fn 外部导入兼容skill写法() {
        let v: serde_json::Value = serde_json::from_str(r#"{
            "title": "A Test Paper", "doi": "https://doi.org/10.1/ABC",
            "authors": ["Chen, L.", "Wang, Y."], "year": 2025,
            "problem": {"text": "问题", "page": 1},
            "method_steps": [{"name": "步骤", "detail": "细节", "page": 3}],
            "findings": [{"metric": "mIoU", "value": "+12.4%", "dataset": "LoveDA", "page": 8}, {"metric": "OA", "value": null}],
            "topics": [{"name": "基础模型", "confidence": 0.9}],
            "source": "workbuddy", "pdf_path": "/tmp/x.pdf"
        }"#).unwrap();
        let (p, pdf) = paper_from_external(&v).unwrap();
        assert_eq!(p.authors, "Chen, L., Wang, Y.");
        assert_eq!(p.doi, "10.1/abc");
        assert_eq!(p.problem, "问题");
        assert_eq!(p.steps[0].name, "步骤");
        assert_eq!(p.findings[0].label, "mIoU · LoveDA");
        assert_eq!(p.findings[1].value, "");
        assert_eq!(p.tags, vec!["基础模型"]);
        assert_eq!(p.origin, "workbuddy");
        assert_eq!(pdf, "/tmp/x.pdf");
        assert!(paper_from_external(&serde_json::json!({"gist": "x"})).is_err());
    }

    #[test]
    fn 合并时空字段不覆盖且保留用户数据() {
        let mut old = Paper { title: "T".into(), gist: "旧要旨".into(), note: "我的笔记".into(), starred: true, tags: vec!["a".into()], ..Default::default() };
        let new = Paper { title: "T".into(), problem: "新问题".into(), tags: vec!["a".into(), "b".into()], ..Default::default() };
        merge_into(&mut old, new);
        assert_eq!(old.gist, "旧要旨");
        assert_eq!(old.problem, "新问题");
        assert_eq!(old.note, "我的笔记");
        assert!(old.starred);
        assert_eq!(old.tags, vec!["a", "b"]);
        assert_eq!(title_key("A Test: Paper!"), title_key("a test paper"));
    }

    #[test]
    fn 按字符截断中文不崩溃() {
        assert_eq!(clip("接口返回错误", 3), "接口返");
    }

    #[test]
    fn 过长的findings值会被整形且数量截断() {
        let mut p = Paper {
            findings: vec![
                Finding {
                    value: "128.9 × 10^4 ha，年均 4.03 × 10^4 ha".into(),
                    label: "扰动面积".into(),
                },
                Finding { value: "a".into(), label: String::new() },
                Finding { value: "b".into(), label: String::new() },
                Finding { value: "c".into(), label: String::new() },
            ],
            tags: vec!["a".into(), "b".into(), "c".into(), "China".into()],
            ..Default::default()
        };
        normalize_findings(&mut p);
        assert_eq!(p.findings.len(), 3);
        assert_eq!(p.findings[0].value, "128.9 × 10^4 ha");
        assert_eq!(p.findings[0].label, "扰动面积");
        assert_eq!(p.tags.len(), 3);
    }
}
