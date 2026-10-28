//! WebDAV 同步。
//!
//! 设计取舍：
//!
//! 1. **不做整库覆盖。** 两台机器各加一篇，覆盖式同步会静默丢掉一篇，
//!    而且用户看不出丢在哪。所以合并到记录级：按 id 取并集。
//! 2. **同一篇以 `rev` 更大的一侧为准。** rev 在 `commit_library` 里统一刷新，
//!    命令层不用各自记得改。
//! 3. **删除用墓碑。** 只删记录不删远端的话，另一台机器下次同步会把它当成
//!    「只存在于远端」再传回来。墓碑优先于存在，代价是「一边删、一边改」
//!    时以删除为准（见 docs/03-sync-spec.md 的取舍说明）。
//! 4. **`file` / `source` 是本机绝对路径**，上传前清空，下载后按 id 重建。
//!    不清空的话，A 机器的 `/Users/a/...` 会漂到 B 机器上覆盖掉能用的路径。
//! 5. **PDF 靠 `pdf-index.json` 判断存在性**，不逐篇 PROPFIND，
//!    避免几百篇文献一次同步打几百个探测请求。
//!
//! 远端目录结构：
//!
//! ```text
//! <用户填的地址>/summa/library.json     文献库（含墓碑）
//! <用户填的地址>/summa/pdf-index.json   有 PDF 的 id 列表
//! <用户填的地址>/summa/files/<id>.pdf   原文
//! ```

use super::*;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tauri::Emitter;

/// 单个请求超时。PDF 上传在慢速网络上可能很久，给足时间。
const DAV_TIMEOUT: u64 = 300;
/// 上传冲突（412）时的重试次数：另一台设备刚好在这几秒里写了远端
const PUT_RETRY: usize = 2;
const PROGRESS_EVENT: &str = "summa://sync";

// ---------- 配置 ----------

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct DavConfig {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    /// 群晖等 NAS 默认用自签名证书，不勾选会直接 TLS 握手失败
    #[serde(default)]
    pub insecure: bool,
    #[serde(default)]
    pub enabled: bool,
    /// 打开应用时自动同步一次
    #[serde(default)]
    pub auto: bool,
}

fn dav_config(app: &tauri::AppHandle) -> DavConfig {
    read_config(app)
        .get("webdav")
        .and_then(|v| serde_json::from_value::<DavConfig>(v.clone()).ok())
        .unwrap_or_default()
}

fn write_dav_config(app: &tauri::AppHandle, cfg: &DavConfig) -> Result<(), String> {
    let mut root = read_config(app);
    root.insert(
        "webdav".into(),
        serde_json::to_value(cfg).map_err(|e| format!("序列化失败：{e}"))?,
    );
    let text = serde_json::to_string_pretty(&root).map_err(|e| format!("序列化失败：{e}"))?;
    std::fs::write(config_path(app)?, text).map_err(|e| format!("保存设置失败：{e}"))
}

fn dav_root(cfg: &DavConfig) -> String {
    format!("{}/summa", cfg.url.trim().trim_end_matches('/'))
}

fn has_password(p: &str) -> bool {
    !p.is_empty()
}

fn mask_password(p: &str) -> String {
    let n = p.chars().count();
    if n == 0 {
        return String::new();
    }
    if n <= 4 {
        return "•".repeat(n);
    }
    // 只回传末两位，够用户确认填的是哪个密码
    let tail: String = p.chars().skip(n - 2).collect();
    format!("••••{tail}")
}

// ---------- 同步状态 ----------
//
// 记下上次同步时远端的 ETag / Last-Modified，用来判断远端有没有变；
// 记下上次上传内容的指纹，用来判断本地有没有变。
// 两边都没变就完全跳过传输——自动同步每 5 分钟跑一次，不做这一步会很费流量。

#[derive(Serialize, Deserialize, Default, Clone)]
struct SyncState {
    #[serde(default)]
    etag: String,
    #[serde(default)]
    last_modified: String,
    #[serde(default)]
    last_sync: u64,
    #[serde(default)]
    payload_hash: u64,
}

fn state_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(data_dir(app)?.join("sync-state.json"))
}

fn read_state(app: &tauri::AppHandle) -> SyncState {
    state_path(app)
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn write_state(app: &tauri::AppHandle, s: &SyncState) -> Result<(), String> {
    let text = serde_json::to_string_pretty(s).map_err(|e| format!("序列化失败：{e}"))?;
    std::fs::write(state_path(app)?, text).map_err(|e| format!("保存同步状态失败：{e}"))
}

/// FNV-1a。只用来判断「内容变没变」，不承担安全用途。
fn fnv64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 上传用的库：清掉本机绝对路径
fn for_remote(lib: &Library) -> Vec<u8> {
    let mut copy = lib.clone();
    for p in copy.papers.iter_mut() {
        p.file.clear();
        p.source.clear();
    }
    serde_json::to_vec_pretty(&copy).unwrap_or_default()
}

// ---------- 合并 ----------

#[derive(Default, Debug)]
struct MergeStats {
    /// 只在远端存在，合并进来
    added: usize,
    /// 两边都有，远端更新，采用远端
    updated: usize,
    /// 命中墓碑，被移除
    deleted: usize,
}

/// 记录级合并。见文件头的四条规则。
fn merge_libraries(local: &Library, remote: &Library) -> (Library, MergeStats) {
    // 1. 墓碑取并集，同 id 取更晚的时间
    let mut tombstones: Vec<Tombstone> = Vec::new();
    for t in local.tombstones.iter().chain(remote.tombstones.iter()) {
        if t.id.is_empty() {
            continue;
        }
        match tombstones.iter_mut().find(|x| x.id == t.id) {
            Some(x) => {
                if t.at > x.at {
                    x.at = t.at;
                }
            }
            None => tombstones.push(t.clone()),
        }
    }
    let dead: HashSet<String> = tombstones.iter().map(|t| t.id.clone()).collect();

    // 2. 本地路径快照。远端记录的路径属于另一台机器，不能拿来覆盖
    let local_path: HashMap<&str, (&str, &str)> = local
        .papers
        .iter()
        .map(|p| (p.id.as_str(), (p.file.as_str(), p.source.as_str())))
        .collect();

    // 3. 取并集
    let mut stats = MergeStats::default();
    let mut order: Vec<String> = Vec::new();
    let mut by_id: HashMap<String, Paper> = HashMap::new();
    for p in &local.papers {
        if p.id.is_empty() || by_id.contains_key(&p.id) {
            continue;
        }
        order.push(p.id.clone());
        by_id.insert(p.id.clone(), p.clone());
    }
    for p in &remote.papers {
        if p.id.is_empty() {
            continue;
        }
        match by_id.get_mut(&p.id) {
            Some(cur) => {
                if sheet_rev(p) > sheet_rev(cur) {
                    *cur = p.clone();
                    stats.updated += 1;
                }
            }
            None => {
                order.push(p.id.clone());
                by_id.insert(p.id.clone(), p.clone());
                stats.added += 1;
            }
        }
    }

    // 4. 去墓碑 + 修路径
    let mut papers = Vec::with_capacity(order.len());
    for id in order {
        if dead.contains(&id) {
            stats.deleted += 1;
            continue;
        }
        let Some(mut p) = by_id.remove(&id) else { continue };
        match local_path.get(id.as_str()) {
            Some((lf, ls)) => {
                // 本地路径能打开就保留；远端带来的路径在本机通常不存在，清掉
                if !lf.is_empty() && Path::new(lf).is_file() {
                    p.file = (*lf).to_string();
                } else if !p.file.is_empty() && !Path::new(&p.file).is_file() {
                    p.file.clear();
                }
                if !ls.is_empty() && Path::new(ls).is_file() {
                    p.source = (*ls).to_string();
                } else if !p.source.is_empty() && !Path::new(&p.source).is_file() {
                    p.source.clear();
                }
            }
            None => {
                // 只来自远端的记录，路径一定不属于本机
                p.file.clear();
                p.source.clear();
            }
        }
        papers.push(p);
    }
    papers.sort_by(|a, b| b.added.cmp(&a.added));

    if tombstones.len() > MAX_TOMBSTONES {
        tombstones.sort_by(|a, b| b.at.cmp(&a.at));
        tombstones.truncate(MAX_TOMBSTONES);
    }
    (Library { papers, tombstones }, stats)
}

// ---------- HTTP ----------

fn dav_client(cfg: &DavConfig) -> Result<reqwest::Client, String> {
    let mut b = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(DAV_TIMEOUT));
    if cfg.insecure {
        b = b.danger_accept_invalid_certs(true);
    }
    b.build().map_err(|e| format!("创建网络客户端失败：{e}"))
}

fn dav_status_error(what: &str, status: reqwest::StatusCode) -> String {
    let hint = match status.as_u16() {
        401 => "用户名或密码不对。群晖要确认「控制面板 → 用户与群组 → 应用程序权限」里 WebDAV 已开放，密码用该账号的应用密码。",
        403 => "这个账号没有该目录的写入权限。",
        404 => "路径不存在，请确认地址指向一个已存在的共享文件夹。",
        409 => "上级目录不存在：地址需要填到一级共享文件夹，例如 https://nas:5006/homes",
        423 => "文件被锁定，可能有其他程序正在占用。",
        507 => "服务器空间不足。",
        _ => "",
    };
    let code = status.as_u16();
    if hint.is_empty() {
        format!("{what}失败（HTTP {code}）")
    } else {
        format!("{what}失败：{hint}（HTTP {code}）")
    }
}

fn dav_net_error(what: &str, e: reqwest::Error, cfg: &DavConfig) -> String {
    if e.is_connect() {
        if cfg.url.trim().starts_with("https") && !cfg.insecure {
            format!("{what}失败：连不上 {}. 群晖默认用自签名证书，请勾选「接受自签名证书」；同时确认端口（HTTPS 默认 5006）。", cfg.url.trim())
        } else {
            format!("{what}失败：连不上服务器，请检查地址、端口和网络。")
        }
    } else if e.is_timeout() {
        format!("{what}超时。文件较大或网络较慢时请稍后重试。")
    } else {
        format!("{what}失败：{e}")
    }
}

async fn dav_mkcol(client: &reqwest::Client, cfg: &DavConfig, url: &str) -> Result<(), String> {
    // reqwest 没有内置 MKCOL，用自定义方法
    let method = reqwest::Method::from_bytes(b"MKCOL").map_err(|e| format!("{e}"))?;
    let resp = client
        .request(method, url)
        .basic_auth(&cfg.username, Some(&cfg.password))
        .send()
        .await
        .map_err(|e| dav_net_error("创建目录", e, cfg))?;
    let status = resp.status();
    // 405 = 已存在，视为成功
    if status.is_success() || status.as_u16() == 405 {
        return Ok(());
    }
    // 服务端常在响应体里说明原因（权限、只读挂载等），带上便于排查
    let body = resp.text().await.unwrap_or_default();
    let detail = body.trim();
    if detail.is_empty() {
        Err(dav_status_error("创建目录", status))
    } else {
        Err(format!(
            "{} 服务端返回：{}",
            dav_status_error("创建目录", status),
            clip(detail, 200)
        ))
    }
}

/// 返回 (etag, last_modified)；远端没有这个文件时返回 None
async fn dav_head(
    client: &reqwest::Client,
    cfg: &DavConfig,
    url: &str,
) -> Result<Option<(String, String)>, String> {
    let resp = client
        .head(url)
        .basic_auth(&cfg.username, Some(&cfg.password))
        .send()
        .await
        .map_err(|e| dav_net_error("读取远端信息", e, cfg))?;
    let status = resp.status();
    if status.as_u16() == 404 {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(dav_status_error("读取远端信息", status));
    }
    let etag = resp
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let lm = resp
        .headers()
        .get(reqwest::header::LAST_MODIFIED)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    Ok(Some((etag, lm)))
}

async fn dav_get(
    client: &reqwest::Client,
    cfg: &DavConfig,
    url: &str,
) -> Result<Option<Vec<u8>>, String> {
    let resp = client
        .get(url)
        .basic_auth(&cfg.username, Some(&cfg.password))
        .send()
        .await
        .map_err(|e| dav_net_error("下载", e, cfg))?;
    let status = resp.status();
    if status.as_u16() == 404 {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(dav_status_error("下载", status));
    }
    let bytes = resp.bytes().await.map_err(|e| dav_net_error("下载", e, cfg))?;
    Ok(Some(bytes.to_vec()))
}

/// 返回 Ok(true) 表示成功；Ok(false) 表示 412（远端在此期间被别人改了）
async fn dav_put(
    client: &reqwest::Client,
    cfg: &DavConfig,
    url: &str,
    body: Vec<u8>,
    if_match: Option<&str>,
) -> Result<bool, String> {
    let mut req = client
        .put(url)
        .basic_auth(&cfg.username, Some(&cfg.password))
        .body(body);
    // 弱验证器（W/"..."）不能用于 If-Match，带上会永远返回 412
    if let Some(tag) = if_match.filter(|t| !t.is_empty() && !t.starts_with("W/")) {
        req = req.header(reqwest::header::IF_MATCH, tag);
    }
    let resp = req.send().await.map_err(|e| dav_net_error("上传", e, cfg))?;
    let status = resp.status();
    if status.as_u16() == 412 {
        return Ok(false);
    }
    if !status.is_success() {
        return Err(dav_status_error("上传", status));
    }
    Ok(true)
}

// ---------- 命令 ----------

#[derive(Serialize)]
pub struct SyncSettings {
    enabled: bool,
    auto: bool,
    url: String,
    username: String,
    has_password: bool,
    password_hint: String,
    insecure: bool,
    last_sync: u64,
}

#[tauri::command]
pub fn get_sync_settings(app: tauri::AppHandle) -> SyncSettings {
    let cfg = dav_config(&app);
    SyncSettings {
        enabled: cfg.enabled,
        auto: cfg.auto,
        url: cfg.url,
        username: cfg.username,
        has_password: has_password(&cfg.password),
        password_hint: mask_password(&cfg.password),
        insecure: cfg.insecure,
        last_sync: read_state(&app).last_sync,
    }
}

/// 保存同步设置。password 为 None 表示不改，空字符串表示清除。
#[tauri::command]
pub fn save_sync_settings(
    app: tauri::AppHandle,
    url: String,
    username: String,
    password: Option<String>,
    insecure: bool,
    enabled: bool,
    auto: bool,
) -> Result<SyncSettings, String> {
    let mut cfg = dav_config(&app);
    let url = url.trim().trim_end_matches('/').to_string();
    if enabled && url.is_empty() {
        return Err("启用同步需要先填写 WebDAV 地址".into());
    }
    if !url.is_empty() && !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("地址要以 http:// 或 https:// 开头".into());
    }
    cfg.url = url;
    cfg.username = username.trim().to_string();
    if let Some(p) = password {
        cfg.password = p.trim().to_string();
    }
    cfg.insecure = insecure;
    cfg.enabled = enabled;
    cfg.auto = auto;
    write_dav_config(&app, &cfg)?;
    Ok(get_sync_settings(app))
}

/// 测试连接：建目录 + 读一次远端库，顺便告诉用户远端已有多少篇
#[tauri::command]
pub async fn test_sync(app: tauri::AppHandle) -> Result<String, String> {
    let cfg = dav_config(&app);
    if cfg.url.trim().is_empty() {
        return Err("请先填写 WebDAV 地址".into());
    }
    let client = dav_client(&cfg)?;
    let root = dav_root(&cfg);
    dav_mkcol(&client, &cfg, &root).await?;
    dav_mkcol(&client, &cfg, &format!("{root}/files")).await?;

    let lib_url = format!("{root}/library.json");
    match dav_get(&client, &cfg, &lib_url).await? {
        Some(raw) => {
            let lib: Library = serde_json::from_slice(&raw)
                .map_err(|e| format!("目录可写，但远端 library.json 解析失败：{e}"))?;
            Ok(format!(
                "连接成功。远端已有 {} 篇文献{}，可以直接同步。",
                lib.papers.len(),
                if lib.tombstones.is_empty() {
                    String::new()
                } else {
                    format!("（含 {} 条删除记录）", lib.tombstones.len())
                }
            ))
        }
        None => Ok("连接成功，目录可写。远端还没有文献库，第一次同步会建立。".into()),
    }
}

#[derive(Serialize, Default)]
pub struct SyncReport {
    at: u64,
    added: usize,
    updated: usize,
    deleted: usize,
    uploaded: bool,
    pdf_up: usize,
    pdf_down: usize,
    message: String,
    library: Option<Library>,
}

fn emit(app: &tauri::AppHandle, stage: &str, detail: &str, done: usize, total: usize) {
    let _ = app.emit(PROGRESS_EVENT, json!({ "stage": stage, "detail": detail, "done": done, "total": total }));
}

fn lock_library() -> std::sync::MutexGuard<'static, ()> {
    LIBRARY_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// 完整的同步流程：拉远端库 → 合并 → 上传 → 同步 PDF → 记录状态。
#[tauri::command]
pub async fn sync_now(app: tauri::AppHandle) -> Result<SyncReport, String> {
    let cfg = dav_config(&app);
    if cfg.url.trim().is_empty() {
        return Err("还没有填写 WebDAV 地址，请先在设置里配置".into());
    }
    if !cfg.enabled {
        return Err("同步未启用".into());
    }

    let client = dav_client(&cfg)?;
    let root = dav_root(&cfg);
    emit(&app, "connect", "正在连接服务器", 0, 0);
    dav_mkcol(&client, &cfg, &root).await?;
    dav_mkcol(&client, &cfg, &format!("{root}/files")).await?;

    let mut state = read_state(&app);
    let mut report = SyncReport { at: now_stamp(), ..Default::default() };
    let local = {
        let _g = lock_library();
        read_library(&app)?
    };

    // ---- 1. 拉远端库并合并 ----
    emit(&app, "pull", "正在读取远端文献库", 0, 0);
    let lib_url = format!("{root}/library.json");
    let mut merged = local.clone();
    let mut remote_etag = String::new();
    let mut remote_missing = true;
    let mut merged_dirty = false;

    if let Some((etag, lm)) = dav_head(&client, &cfg, &lib_url).await? {
        remote_missing = false;
        remote_etag = etag.clone();
        let unchanged =
            (!etag.is_empty() && etag == state.etag) || (etag.is_empty() && !lm.is_empty() && lm == state.last_modified);
        if !unchanged {
            if let Some(raw) = dav_get(&client, &cfg, &lib_url).await? {
                let remote: Library = serde_json::from_slice(&raw).map_err(|e| {
                    format!("服务器上的 library.json 无法解析（{e}）。已中止同步，本地数据没有改动。")
                })?;
                let (m, stats) = merge_libraries(&local, &remote);
                merged = m;
                merged_dirty = true;
                report.added = stats.added;
                report.updated = stats.updated;
                report.deleted = stats.deleted;
            }
        }
    }

    // 合并结果与本地不同才落盘；这里不走 commit_library，
    // 因为 rev 已经在各自设备上定好了，重新刷会破坏「谁更新」的判定。
    let local_text = serde_json::to_string(&local).unwrap_or_default();
    let merged_text = serde_json::to_string(&merged).unwrap_or_default();
    if local_text != merged_text {
        let _g = lock_library();
        write_library(&app, &merged)?;
    }

    // ---- 2. 上传（内容有变化才传）----
    let payload = for_remote(&merged);
    let hash = fnv64(&payload);
    if hash != state.payload_hash || remote_missing || merged_dirty {
        emit(&app, "push", "正在上传文献库", 0, 0);
        let mut ok = false;
        for attempt in 0..PUT_RETRY {
            let body = for_remote(&merged);
            if dav_put(&client, &cfg, &lib_url, body, Some(&remote_etag)).await? {
                ok = true;
                break;
            }
            // 412：远端在这几秒里被另一台设备改了，重新拉一次再合并
            if attempt + 1 == PUT_RETRY {
                break;
            }
            if let Some(raw) = dav_get(&client, &cfg, &lib_url).await? {
                if let Ok(remote) = serde_json::from_slice::<Library>(&raw) {
                    let (m, stats) = merge_libraries(&merged, &remote);
                    merged = m;
                    report.updated += stats.updated;
                    report.added += stats.added;
                }
            }
            if let Some((etag, _)) = dav_head(&client, &cfg, &lib_url).await? {
                remote_etag = etag;
            }
        }
        if !ok {
            return Err("另一台设备正在同步，稍后再试一次".into());
        }
        report.uploaded = true;
    }

    // ---- 3. 同步 PDF ----
    let files_dir = data_dir(&app)?.join("files");
    std::fs::create_dir_all(&files_dir).map_err(|e| format!("创建目录失败：{e}"))?;
    let idx_url = format!("{root}/pdf-index.json");
    let mut remote_ids: HashSet<String> = match dav_get(&client, &cfg, &idx_url).await? {
        Some(raw) => serde_json::from_slice::<Vec<String>>(&raw).unwrap_or_default().into_iter().collect(),
        None => HashSet::new(),
    };
    let before_ids = remote_ids.len();

    // 本机有、远端没有 → 上传
    let uploads: Vec<(String, PathBuf)> = merged
        .papers
        .iter()
        .filter(|p| !p.file.is_empty() && !remote_ids.contains(&p.id))
        .filter_map(|p| {
            let path = PathBuf::from(&p.file);
            path.is_file().then(|| (p.id.clone(), path))
        })
        .collect();

    let total_pdf = uploads.len();
    for (i, (id, path)) in uploads.iter().enumerate() {
        emit(&app, "pdf-up", &format!("上传原文 {}", id), i + 1, total_pdf);
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        dav_put(&client, &cfg, &format!("{root}/files/{id}.pdf"), bytes, None).await?;
        remote_ids.insert(id.clone());
        report.pdf_up += 1;
    }

    // 远端有、本机没有 → 下载（墓碑或已删除的 id 不下）
    let live: HashSet<String> = merged.papers.iter().map(|p| p.id.clone()).collect();
    let missing: Vec<String> = remote_ids
        .iter()
        .filter(|id| live.contains(*id))
        .filter(|id| !files_dir.join(format!("{id}.pdf")).is_file())
        .cloned()
        .collect();

    let total_down = missing.len();
    let mut path_updates = false;
    for (i, id) in missing.iter().enumerate() {
        emit(&app, "pdf-down", &format!("下载原文 {id}"), i + 1, total_down);
        let Some(bytes) = dav_get(&client, &cfg, &format!("{root}/files/{id}.pdf")).await? else {
            continue;
        };
        let dest = files_dir.join(format!("{id}.pdf"));
        std::fs::write(&dest, bytes).map_err(|e| format!("写入 PDF 失败：{e}"))?;
        if let Some(p) = merged.papers.iter_mut().find(|p| &p.id == id) {
            p.file = dest.to_string_lossy().into_owned();
            path_updates = true;
        }
        report.pdf_down += 1;
    }

    if path_updates {
        let _g = lock_library();
        write_library(&app, &merged)?;
    }

    // 索引收窄到仍在库里的 id，顺便清理远端没有对应文献的条目
    remote_ids.retain(|id| live.contains(id));
    if remote_ids.len() != before_ids || report.pdf_up > 0 {
        let mut ids: Vec<String> = remote_ids.into_iter().collect();
        ids.sort();
        let body = serde_json::to_vec_pretty(&ids).unwrap_or_default();
        dav_put(&client, &cfg, &idx_url, body, None).await?;
    }

    // ---- 4. 记录状态 ----
    let (final_etag, final_lm) = dav_head(&client, &cfg, &lib_url).await?.unwrap_or_default();
    let final_payload = for_remote(&merged);
    state.etag = final_etag;
    state.last_modified = final_lm;
    state.last_sync = now_stamp();
    state.payload_hash = fnv64(&final_payload);
    write_state(&app, &state)?;

    report.message = summarize(&report);
    report.library = Some(merged);
    emit(&app, "done", &report.message, 1, 1);
    Ok(report)
}

fn summarize(r: &SyncReport) -> String {
    let mut parts: Vec<String> = Vec::new();
    if r.added > 0 {
        parts.push(format!("新增 {} 篇", r.added));
    }
    if r.updated > 0 {
        parts.push(format!("更新 {} 篇", r.updated));
    }
    if r.deleted > 0 {
        parts.push(format!("按另一台设备的删除移除 {} 篇", r.deleted));
    }
    if r.pdf_up > 0 {
        parts.push(format!("上传原文 {} 篇", r.pdf_up));
    }
    if r.pdf_down > 0 {
        parts.push(format!("下载原文 {} 篇", r.pdf_down));
    }
    if parts.is_empty() {
        "已是最新，没有需要同步的内容".to_string()
    } else {
        format!("同步完成：{}", parts.join("，"))
    }
}

// ---------- 测试 ----------

#[cfg(test)]
mod tests {
    use super::*;

    fn paper(id: &str, rev: u64, gist: &str) -> Paper {
        Paper { id: id.into(), title: format!("T{id}"), gist: gist.into(), rev, added: rev, ..Default::default() }
    }

    #[test]
    fn 两边各加一篇合并后都在() {
        let local = Library { papers: vec![paper("a", 100, "本地")], tombstones: vec![] };
        let remote = Library { papers: vec![paper("b", 200, "远端")], tombstones: vec![] };
        let (m, stats) = merge_libraries(&local, &remote);
        assert_eq!(m.papers.len(), 2);
        assert_eq!(stats.added, 1);
        assert!(m.papers.iter().any(|p| p.id == "a"));
        assert!(m.papers.iter().any(|p| p.id == "b"));
    }

    #[test]
    fn 同一篇以rev大的一侧为准() {
        let local = Library { papers: vec![paper("a", 100, "旧")], tombstones: vec![] };
        let remote = Library { papers: vec![paper("a", 300, "新")], tombstones: vec![] };
        let (m, _) = merge_libraries(&local, &remote);
        assert_eq!(m.papers.len(), 1);
        assert_eq!(m.papers[0].gist, "新");

        // 反过来本地更新，不该被远端旧数据覆盖
        let (m2, _) = merge_libraries(&remote, &local);
        assert_eq!(m2.papers[0].gist, "新");
    }

    #[test]
    fn 墓碑优先于存在() {
        let local = Library { papers: vec![], tombstones: vec![Tombstone { id: "a".into(), at: 500 }] };
        let remote = Library { papers: vec![paper("a", 100, "远端还在")], tombstones: vec![] };
        let (m, stats) = merge_libraries(&local, &remote);
        assert!(m.papers.is_empty(), "被删过的文献不应该被另一台机器带回来");
        assert_eq!(stats.deleted, 1);
        assert_eq!(m.tombstones.len(), 1, "墓碑要保留，否则下次又被复活");
    }

    #[test]
    fn 墓碑并集同id取更晚时间() {
        let local = Library { papers: vec![], tombstones: vec![Tombstone { id: "a".into(), at: 100 }] };
        let remote = Library { papers: vec![], tombstones: vec![Tombstone { id: "a".into(), at: 900 }, Tombstone { id: "b".into(), at: 50 }] };
        let (m, _) = merge_libraries(&local, &remote);
        assert_eq!(m.tombstones.len(), 2);
        assert_eq!(m.tombstones.iter().find(|t| t.id == "a").unwrap().at, 900);
    }

    #[test]
    fn 远端带来的本机路径会被清掉() {
        let mut l = paper("a", 100, "本地");
        l.file = "/tmp/summa-test-nonexistent.pdf".into();
        l.source = "/tmp/summa-test-nonexistent.pdf".into();
        let local = Library { papers: vec![l], tombstones: vec![] };

        let mut r = paper("b", 200, "远端");
        r.file = "/Users/someone-else/Library/files/b.pdf".into();
        r.source = "/Users/someone-else/Downloads/b.pdf".into();
        let remote = Library { papers: vec![r], tombstones: vec![] };

        let (m, _) = merge_libraries(&local, &remote);
        let b = m.papers.iter().find(|p| p.id == "b").unwrap();
        assert!(b.file.is_empty(), "另一台机器的绝对路径不能留");
        assert!(b.source.is_empty());
    }

    #[test]
    fn 上传前清空本机路径且不改动原库() {
        let mut p = paper("a", 100, "x");
        p.file = "/Users/me/files/a.pdf".into();
        let lib = Library { papers: vec![p], tombstones: vec![] };
        let bytes = for_remote(&lib);
        let sent: Library = serde_json::from_slice(&bytes).unwrap();
        assert!(sent.papers[0].file.is_empty());
        assert_eq!(lib.papers[0].file, "/Users/me/files/a.pdf", "原库不能被改");
    }

    #[test]
    fn 老数据没有rev时用入库时间兜底() {
        let p = Paper { id: "a".into(), added: 777, ..Default::default() };
        assert_eq!(sheet_rev(&p), 777);
        let p2 = Paper { id: "a".into(), added: 100, rev: 900, ..Default::default() };
        assert_eq!(sheet_rev(&p2), 900);
    }

    /// 对真实 WebDAV 服务验证 HTTP 语义。没有设置 SUMMA_DAV_TEST_URL 时直接跳过，
    /// 所以不会影响普通 `cargo test`。
    ///
    /// 这层必须实跑：MKCOL 的「已存在」状态码、HEAD 是否返回 ETag、
    /// If-Match 冲突是否返回 412，各家实现差异很大，编译期看不出来。
    ///
    /// ```bash
    /// SUMMA_DAV_TEST_URL=http://127.0.0.1:8099 cargo test webdav
    /// ```
    #[test]
    fn webdav基础操作() {
        let Ok(url) = std::env::var("SUMMA_DAV_TEST_URL") else {
            return;
        };
        if url.trim().is_empty() {
            return;
        }
        // 本机地址不走代理：reqwest 默认读 HTTP_PROXY，
        // 某些环境里代理会把请求转成绝对 URI，服务器会直接 400
        std::env::set_var("NO_PROXY", "127.0.0.1,localhost");
        std::env::set_var("no_proxy", "127.0.0.1,localhost");
        let cfg = DavConfig {
            url: url.trim().to_string(),
            username: std::env::var("SUMMA_DAV_TEST_USER").unwrap_or_default(),
            password: std::env::var("SUMMA_DAV_TEST_PASS").unwrap_or_default(),
            insecure: false,
            enabled: true,
            auto: false,
        };

        let res: Result<(), String> = tauri::async_runtime::block_on(async {
            let client = dav_client(&cfg)?;
            let root = dav_root(&cfg);
            dav_mkcol(&client, &cfg, &root).await?;
            // 第二次应该命中 405「已存在」而不是报错
            dav_mkcol(&client, &cfg, &root).await?;
            dav_mkcol(&client, &cfg, &format!("{root}/files")).await?;

            let file = format!("{root}/library.json");
            let body = br#"{"papers":[],"tombstones":[]}"#.to_vec();
            assert!(dav_put(&client, &cfg, &file, body.clone(), None).await?, "首次上传应成功");

            let (etag, _lm) = dav_head(&client, &cfg, &file).await?.expect("远端应存在");
            assert!(!etag.is_empty(), "服务端应返回 ETag");

            let got = dav_get(&client, &cfg, &file).await?.expect("远端应存在");
            assert_eq!(got, body, "下载内容应与上传一致");

            // 过期的 If-Match 必须被拒绝，否则并发写会互相覆盖
            let stale = dav_put(&client, &cfg, &file, b"{\"papers\":[]}".to_vec(), Some("\"stale-etag\"")).await?;
            assert!(!stale, "过期的 If-Match 应返回 412");

            let ok = dav_put(&client, &cfg, &file, b"{\"papers\":[]}".to_vec(), Some(&etag)).await?;
            assert!(ok, "正确的 If-Match 应成功");

            // 不存在的文件应返回 None 而不是报错
            assert!(dav_get(&client, &cfg, &format!("{root}/nope.json")).await?.is_none());
            assert!(dav_head(&client, &cfg, &format!("{root}/nope.json")).await?.is_none());
            Ok(())
        });
        res.unwrap();
    }
}
