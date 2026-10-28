//! WebDAV 同步。
//!
//! 设计参考了 Obsidian 插件 remotely-save 的 v3 同步算法
//! （<https://github.com/remotely-save/remotely-save>），核心有两条：
//!
//! 1. **三路比对**：拿「上次同步成功的快照」当第三个输入，
//!    才能区分「本地删掉了」和「本地从来没有过」。
//! 2. **同步状态只存本地**，不往远端写派生元数据。远端只有用户真正要的
//!    东西（库和 PDF），出问题时可以手动查看和处理。
//!
//! 为什么不用「覆盖」或「墓碑」：
//!
//! - 整库覆盖：两台各加一篇，后同步的会把先同步的整库覆盖掉，**静默丢数据**。
//! - 墓碑标记删除：墓碑优先于存在，于是「A 机删了、B 机恰好改了」时，
//!   B 的修改会被无声丢弃。三路比对能看出 B 是「改过」而不是「没动过」，
//!   这种冲突可以保留两份而不是丢掉一份。
//!
//! 冲突用「保留两份」而不是「较新者胜」：文献笔记是用户自己整理的东西，
//! 自动丢弃一份等于替他做了决定。副本按 rev 决胜生成固定 id，
//! 两端算出的结果一致，不会越同步越多。
//!
//! 远端布局：
//!
//! ```text
//! <用户填的地址>/summa/library.json     文献库
//! <用户填的地址>/summa/pdf-index.json   有 PDF 的 id 列表（纯缓存，丢了会自动重建）
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

/// 删除保护：一次同步要删掉的数量超过阈值就中止。
/// 远端文件被误删、库文件损坏、或者代码有 bug 时，这是最后一道闸。
/// 正常情况下一次同步不会删掉十几篇文献。
const DELETE_GUARD_ABS: usize = 12;
const DELETE_GUARD_RATIO: f64 = 0.3;

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
// 全部存在本机，不上传。四项各有用途：
//
// - `snapshot`：上次同步成功时每个 id 的 rev，三路比对的第三个输入。
//   这是整个同步正确性的基础，丢了会退化成「并集 + 较新者胜」。
// - `etag` / `last_modified`：判断远端有没有变，没变就不下载。
// - `payload_hash`：判断本地有没有变，没变就不上传。
//
// 自动同步每次开应用都跑，不做这两项判断会白白传输几百 KB 到几 MB。

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
    /// id -> rev。老版本升级上来时为空，这时退化为并集合并，
    /// 且必须强制拉一次远端，否则会把本地 rev 当成远端状态写进快照。
    #[serde(default)]
    snapshot: HashMap<String, u64>,
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

/// 由文献库算出快照。同步成功后调用。
fn snapshot_of(lib: &Library) -> HashMap<String, u64> {
    lib.papers.iter().map(|p| (p.id.clone(), sheet_rev(p))).collect()
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

// ---------- 三路比对 ----------

/// 一条记录相对「上次同步快照」的状态。
///
/// `Created` 和 `Unchanged` 必须分开：只看两边有没有这条记录，
/// 判断不了「本地没有」是「删掉了」还是「从来没有过」，
/// 前者要删远端，后者要拉下来。
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Side {
    /// 快照里也没有
    Nowhere,
    /// 快照里没有，现在有
    Created,
    /// 快照里有，现在也有，rev 未变
    Unchanged,
    /// 快照里有，现在也有，rev 变了
    Modified,
    /// 快照里有，现在没有了
    Deleted,
}

fn side_state(now: Option<u64>, snap: Option<u64>) -> Side {
    match (now, snap) {
        (Some(_), None) => Side::Created,
        (Some(n), Some(s)) if n == s => Side::Unchanged,
        (Some(_), Some(_)) => Side::Modified,
        (None, Some(_)) => Side::Deleted,
        (None, None) => Side::Nowhere,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Act {
    /// 本地优先，必要时上传
    KeepLocal,
    /// 采用远端
    Pull,
    /// 远端删了、本地没动 → 删本地
    DeleteLocal,
    /// 本地删了、远端没动 → 删远端
    DeleteRemote,
    /// 两边都改了 → 保留两份
    Conflict,
    /// 两边都没了
    Drop,
}

/// 决策矩阵。行是本地状态，列是远端状态。
///
/// 两处非直觉的地方都是刻意的：
/// - `Modified × Deleted` 取本地：本地改过说明用户还要它，
///   远端那次删除很可能是在另一台机器上误删的。
/// - `Deleted × Modified` 取远端：对称地，远端改过说明它还有价值。
///
/// 这两条替代了墓碑机制。旧方案里墓碑优先于存在，前一种情况会丢修改。
fn decide(local: Side, remote: Side) -> Act {
    use Act::*;
    use Side::*;

    // `Nowhere` = 两边都没有、快照里也没有。这时只看另一边：
    // 另一边有就同步过去，两边都没有才丢弃。
    // 这里不能一律丢弃——本地刚新增、远端还没有时正是 `Created × Nowhere`。
    match (local, remote) {
        (Nowhere, Nowhere) | (Nowhere, Deleted) | (Deleted, Nowhere) => return Drop,
        (Nowhere, _) => return Pull,
        (_, Nowhere) => return KeepLocal,
        _ => {}
    }

    match (local, remote) {
        (Unchanged, Unchanged) => KeepLocal,
        (Unchanged, Modified) => Pull,
        (Unchanged, Deleted) => DeleteLocal,
        (Unchanged, Created) => Pull,

        (Modified, Unchanged) => KeepLocal,
        (Modified, Modified) => Conflict,
        (Modified, Deleted) => KeepLocal,
        (Modified, Created) => KeepLocal,

        (Deleted, Unchanged) => DeleteRemote,
        (Deleted, Modified) => Pull,
        (Deleted, Deleted) => Drop,
        (Deleted, Created) => Pull,

        (Created, Unchanged) => KeepLocal,
        (Created, Modified) => Conflict,
        (Created, Deleted) => KeepLocal,
        (Created, Created) => Conflict,

        // 上面已经 return 完了，这里只为让 match 穷尽
        (Nowhere, _) | (_, Nowhere) => Drop,
    }
}

#[derive(Default, Debug)]
struct MergeStats {
    /// 远端有、本地没有，拉下来
    added: usize,
    /// 两端都有，采用远端版本
    updated: usize,
    /// 本地被删除（远端删了）
    deleted: usize,
    /// 远端被删除（本地删了）
    remote_deleted: usize,
    /// 两边都改了，保留两份
    conflicts: usize,
}

/// 远端记录带的是另一台机器的绝对路径，不能拿来覆盖本地能用的路径。
///
/// A 机上传时 `for_remote` 会清空路径，但两边都改过时可能取到远端版本，
/// 所以这里再兜一次：本地路径能打开就保留。
fn adopt_local_paths(p: &mut Paper, local: Option<(&str, &str)>) {
    match local {
        Some((lf, ls)) => {
            if !lf.is_empty() && Path::new(lf).is_file() {
                p.file = lf.to_string();
            } else if !p.file.is_empty() && !Path::new(&p.file).is_file() {
                p.file.clear();
            }
            if !ls.is_empty() && Path::new(ls).is_file() {
                p.source = ls.to_string();
            } else if !p.source.is_empty() && !Path::new(&p.source).is_file() {
                p.source.clear();
            }
        }
        None => {
            p.file.clear();
            p.source.clear();
        }
    }
}

/// 冲突时按 rev 决胜，rev 相同则比内容。
///
/// 必须两端算出同一个结果——否则 A 保留本地、B 也保留本地，
/// 同步几次之后两边各有一份「原件」，副本越滚越多。
fn pick_winner<'a>(a: &'a Paper, b: &'a Paper) -> (&'a Paper, &'a Paper) {
    let (ra, rb) = (sheet_rev(a), sheet_rev(b));
    if ra != rb {
        return if ra > rb { (a, b) } else { (b, a) };
    }
    let sa = serde_json::to_string(a).unwrap_or_default();
    let sb = serde_json::to_string(b).unwrap_or_default();
    if sa >= sb {
        (a, b)
    } else {
        (b, a)
    }
}

/// 冲突副本：id 由败者的 rev 决定，两端算出的 id 相同，
/// 于是下次同步会把它识别成「同一篇」，不会再多生成一份。
fn conflict_copy(winner: &Paper, loser: &Paper) -> Paper {
    let mut c = loser.clone();
    let at = sheet_rev(winner);
    c.id = format!("{}c{}", loser.id, sheet_rev(loser));
    c.title = format!("{}（冲突副本）", loser.title);
    // rev 和 added 都取胜者的修改时间：
    // - rev 让它和原件的「新旧」一致，不会被判成更旧的版本
    // - added 让它在「最近添加」排序里靠前，用户能看见
    // 注意 id 里的数字用的是败者 rev，那是身份标识，与 rev 无关
    c.rev = at;
    c.added = at;
    c
}

/// 相对上次同步，有多少篇会消失。
///
/// 用快照当基线而不是「本地删的 + 远端删的」，是为了覆盖
/// 「远端没变、本地被清空」这种情况——那时根本没有做过合并。
fn count_removals(snapshot: &HashMap<String, u64>, merged: &Library) -> usize {
    if snapshot.is_empty() {
        return 0; // 没有快照就判断不了删除，也就无所谓保护
    }
    let live: HashSet<&str> = merged.papers.iter().map(|p| p.id.as_str()).collect();
    snapshot.keys().filter(|id| !live.contains(id.as_str())).count()
}

/// 三路合并。`snapshot` 是上次同步成功时的 id -> rev。
fn merge_three_way(local: &Library, remote: &Library, snapshot: &HashMap<String, u64>) -> (Library, MergeStats) {
    let local_path: HashMap<&str, (&str, &str)> = local
        .papers
        .iter()
        .map(|p| (p.id.as_str(), (p.file.as_str(), p.source.as_str())))
        .collect();
    let lmap: HashMap<&str, &Paper> = local.papers.iter().map(|p| (p.id.as_str(), p)).collect();
    let rmap: HashMap<&str, &Paper> = remote.papers.iter().map(|p| (p.id.as_str(), p)).collect();

    let mut order: Vec<String> = Vec::new();
    let mut known: HashSet<String> = HashSet::new();
    for p in local.papers.iter().chain(remote.papers.iter()) {
        if !p.id.is_empty() && known.insert(p.id.clone()) {
            order.push(p.id.clone());
        }
    }

    let mut stats = MergeStats::default();
    let mut papers: Vec<Paper> = Vec::new();
    let mut copies: Vec<Paper> = Vec::new();

    for id in order {
        let l = lmap.get(id.as_str()).copied();
        let r = rmap.get(id.as_str()).copied();
        let snap = snapshot.get(&id).copied();
        let act = decide(side_state(l.map(sheet_rev), snap), side_state(r.map(sheet_rev), snap));
        let paths = local_path.get(id.as_str()).copied();

        match act {
            Act::Drop => {}
            Act::KeepLocal => {
                if let Some(p) = l {
                    let mut p = p.clone();
                    adopt_local_paths(&mut p, paths);
                    papers.push(p);
                }
            }
            Act::Pull => {
                if let Some(p) = r {
                    let mut p = p.clone();
                    adopt_local_paths(&mut p, paths);
                    papers.push(p);
                    if l.is_some() {
                        stats.updated += 1;
                    } else {
                        stats.added += 1;
                    }
                }
            }
            Act::DeleteLocal => stats.deleted += 1,
            Act::DeleteRemote => stats.remote_deleted += 1,
            Act::Conflict => {
                if let (Some(lp), Some(rp)) = (l, r) {
                    let (win, lose) = pick_winner(lp, rp);
                    let mut w = win.clone();
                    adopt_local_paths(&mut w, paths);
                    papers.push(w);
                    let copy = conflict_copy(win, lose);
                    // 上一次同步可能已经为同一个冲突生成过这个 id
                    if !known.contains(&copy.id) {
                        copies.push(copy);
                    }
                    stats.conflicts += 1;
                } else if let Some(p) = l.or(r) {
                    // 理论上到不了这里；保守起见保留，不丢数据
                    let mut p = p.clone();
                    adopt_local_paths(&mut p, paths);
                    papers.push(p);
                }
            }
        }
    }

    // 副本放最前，用户一眼能看到
    for c in copies.into_iter().rev() {
        papers.insert(0, c);
    }
    papers.sort_by(|a, b| b.added.cmp(&a.added));

    (Library { papers }, stats)
}

/// 没有快照时的退化方案：并集 + 较新者胜。
///
/// 用于两种情况：第一次同步，以及用户删了 `sync-state.json`。
/// 这时判断不了删除，所以保守地既不删本地也不删远端，只做合并。
fn merge_bootstrap(local: &Library, remote: &Library) -> (Library, MergeStats) {
    let local_path: HashMap<&str, (&str, &str)> = local
        .papers
        .iter()
        .map(|p| (p.id.as_str(), (p.file.as_str(), p.source.as_str())))
        .collect();

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

    let mut papers = Vec::with_capacity(order.len());
    for id in order {
        let Some(mut p) = by_id.remove(&id) else { continue };
        adopt_local_paths(&mut p, local_path.get(id.as_str()).copied());
        papers.push(p);
    }
    papers.sort_by(|a, b| b.added.cmp(&a.added));
    (Library { papers }, stats)
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
        has_password: !cfg.password.is_empty(),
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
                "连接成功。远端已有 {} 篇文献，可以直接同步。",
                lib.papers.len()
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
    conflicts: usize,
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

/// 完整的同步流程：拉远端库 → 三路合并 → 上传 → 同步 PDF → 记录快照。
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
        // 快照为空时必须真拉一次：否则会把本地 rev 当成远端状态写进快照，
        // 下次同步就会误判「远端改过」
        let unchanged = !state.snapshot.is_empty()
            && ((!etag.is_empty() && etag == state.etag)
                || (etag.is_empty() && !lm.is_empty() && lm == state.last_modified));
        if !unchanged {
            if let Some(raw) = dav_get(&client, &cfg, &lib_url).await? {
                let remote: Library = serde_json::from_slice(&raw).map_err(|e| {
                    format!("服务器上的 library.json 无法解析（{e}）。已中止同步，本地数据没有改动。")
                })?;
                let (m, stats) = if state.snapshot.is_empty() {
                    merge_bootstrap(&local, &remote)
                } else {
                    merge_three_way(&local, &remote, &state.snapshot)
                };
                merged = m;
                merged_dirty = true;
                report.added = stats.added;
                report.updated = stats.updated;
                report.deleted = stats.deleted;
                report.conflicts = stats.conflicts;
            }
        }
    }

    // 删除保护：与上次同步的快照比，有多少篇会消失。
    //
    // 必须在「是否拉取远端」之外单独算：远端没变时会跳过合并，
    // 此时若本地 library.json 损坏或被清空，会直接把空库推上去，
    // 远端跟着被抹掉——这正是这条闸要拦的情况。
    let removing = count_removals(&state.snapshot, &merged);
    let baseline = state.snapshot.len().max(merged.papers.len());
    if removing > DELETE_GUARD_ABS && (removing as f64) > (baseline as f64) * DELETE_GUARD_RATIO {
        return Err(format!(
            "本次同步会删除 {removing} 篇（上次同步有 {} 篇），超过安全阈值，已中止，远端和本地都没有改动。\n\n\
             常见原因：本地 library.json 被清空或替换、另一台设备重置了文献库、远端文件被误改。\n\n\
             如果本地文献确实丢了，想从服务器恢复：完全退出应用，删除数据目录里的 sync-state.json，\
             再打开同步一次，会用远端的内容重建本地库。",
            baseline
        ));
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
                    let (m, stats) = if state.snapshot.is_empty() {
                        merge_bootstrap(&merged, &remote)
                    } else {
                        merge_three_way(&merged, &remote, &state.snapshot)
                    };
                    merged = m;
                    report.added += stats.added;
                    report.updated += stats.updated;
                    report.conflicts += stats.conflicts;
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

    // 远端有、本机没有 → 下载
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

    // 索引收窄到仍在库里的 id
    remote_ids.retain(|id| live.contains(id));
    if remote_ids.len() != before_ids || report.pdf_up > 0 {
        let mut ids: Vec<String> = remote_ids.into_iter().collect();
        ids.sort();
        let body = serde_json::to_vec_pretty(&ids).unwrap_or_default();
        dav_put(&client, &cfg, &idx_url, body, None).await?;
    }

    // ---- 4. 记录状态 ----
    let (final_etag, final_lm) = dav_head(&client, &cfg, &lib_url).await?.unwrap_or_default();
    state.etag = final_etag;
    state.last_modified = final_lm;
    state.last_sync = now_stamp();
    state.payload_hash = fnv64(&for_remote(&merged));
    state.snapshot = snapshot_of(&merged);
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
        parts.push(format!("远端已删除，同步移除 {} 篇", r.deleted));
    }
    if r.conflicts > 0 {
        parts.push(format!("{} 处冲突已各保留两份", r.conflicts));
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

    fn paper(id: &str, rev: u64) -> Paper {
        Paper {
            id: id.into(),
            title: format!("T{id}"),
            gist: format!("rev{rev}"),
            rev,
            added: rev,
            ..Default::default()
        }
    }

    fn lib(papers: Vec<Paper>) -> Library {
        Library { papers }
    }

    fn snap(items: &[(&str, u64)]) -> HashMap<String, u64> {
        items.iter().map(|(id, rev)| (id.to_string(), *rev)).collect()
    }

    fn ids(l: &Library) -> Vec<String> {
        let mut v: Vec<String> = l.papers.iter().map(|p| p.id.clone()).collect();
        v.sort();
        v
    }

    #[test]
    fn 无变化时不产生动作() {
        let (m, st) = merge_three_way(&lib(vec![paper("a", 100)]), &lib(vec![paper("a", 100)]), &snap(&[("a", 100)]));
        assert_eq!(m.papers.len(), 1);
        assert_eq!((st.added, st.updated, st.deleted, st.remote_deleted, st.conflicts), (0, 0, 0, 0, 0));
    }

    #[test]
    fn 两边各新增一篇都保留() {
        let l = lib(vec![paper("a", 100), paper("b", 200)]);
        let r = lib(vec![paper("a", 100), paper("c", 300)]);
        let (m, st) = merge_three_way(&l, &r, &snap(&[("a", 100)]));
        assert_eq!(ids(&m), vec!["a", "b", "c"]);
        assert_eq!(st.added, 1);
        assert_eq!(st.conflicts, 0);
    }

    #[test]
    fn 单边修改直接采用() {
        let (m, st) = merge_three_way(&lib(vec![paper("a", 100)]), &lib(vec![paper("a", 900)]), &snap(&[("a", 100)]));
        assert_eq!(m.papers[0].gist, "rev900");
        assert_eq!(st.updated, 1);
        assert_eq!(st.conflicts, 0);
    }

    #[test]
    fn 双边修改保留两份且两端结果一致() {
        let l = lib(vec![paper("a", 500)]);
        let r = lib(vec![paper("a", 900)]);
        let s = snap(&[("a", 100)]);

        let (m1, st1) = merge_three_way(&l, &r, &s);
        assert_eq!(st1.conflicts, 1);
        assert_eq!(m1.papers.len(), 2, "冲突应产生一份副本");
        assert_eq!(ids(&m1), vec!["a", "ac500"]);

        // 反过来算一次（模拟另一台设备），必须得到同一组 id
        let (m2, _) = merge_three_way(&r, &l, &s);
        assert_eq!(ids(&m1), ids(&m2), "两端必须算出同样的结果，否则副本会越滚越多");
        assert_eq!(m1.papers.iter().find(|p| p.id == "a").unwrap().gist, "rev900", "新的一版保住原 id");
    }

    /// 另一台设备已经生成过副本时，本机不再生成第二份
    #[test]
    fn 已存在的冲突副本不会重复生成() {
        // B 机：本地是旧版 a(500)，快照里只有 a(100)
        let l = lib(vec![paper("a", 500)]);
        // A 机同步后远端已有胜者 a(900) 和 A 生成的副本 ac500
        let r = lib(vec![paper("a", 900), paper("ac500", 500)]);
        let (m, st) = merge_three_way(&l, &r, &snap(&[("a", 100)]));
        assert_eq!(st.conflicts, 1, "仍应报告冲突");
        assert_eq!(ids(&m), vec!["a", "ac500"], "不应再多出一份副本");
        assert_eq!(m.papers.iter().find(|p| p.id == "a").unwrap().gist, "rev900");
    }

    #[test]
    fn 远端删除会移除本地未改动的() {
        let l = lib(vec![paper("a", 100), paper("b", 100)]);
        let r = lib(vec![paper("a", 100)]);
        let (m, st) = merge_three_way(&l, &r, &snap(&[("a", 100), ("b", 100)]));
        assert_eq!(ids(&m), vec!["a"]);
        assert_eq!(st.deleted, 1);
    }

    #[test]
    fn 本地删除会移除远端未改动的() {
        let l = lib(vec![paper("a", 100)]);
        let r = lib(vec![paper("a", 100), paper("b", 100)]);
        let (m, st) = merge_three_way(&l, &r, &snap(&[("a", 100), ("b", 100)]));
        assert_eq!(ids(&m), vec!["a"]);
        assert_eq!(st.remote_deleted, 1);
    }

    /// 旧的墓碑方案会在这里丢数据：一边删、一边改
    #[test]
    fn 远端删除但本地改过则保留修改() {
        let (m, st) = merge_three_way(&lib(vec![paper("a", 800)]), &lib(vec![]), &snap(&[("a", 100)]));
        assert_eq!(m.papers.len(), 1, "本地改过的不能被远端删除带走");
        assert_eq!(m.papers[0].gist, "rev800");
        assert_eq!(st.deleted, 0);
    }

    #[test]
    fn 本地删除但远端改过则取远端() {
        let (m, st) = merge_three_way(&lib(vec![]), &lib(vec![paper("a", 800)]), &snap(&[("a", 100)]));
        assert_eq!(m.papers.len(), 1);
        assert_eq!(m.papers[0].gist, "rev800");
        assert_eq!(st.remote_deleted, 0);
    }

    #[test]
    fn 两边都删除时静默丢弃() {
        let (m, st) = merge_three_way(&lib(vec![]), &lib(vec![]), &snap(&[("a", 100)]));
        assert!(m.papers.is_empty());
        assert_eq!((st.deleted, st.remote_deleted, st.conflicts), (0, 0, 0));
    }

    /// 没有快照时退化为并集，绝不删东西
    #[test]
    fn 空快照退化为并集不删除() {
        let l = lib(vec![paper("a", 100), paper("b", 200)]);
        let r = lib(vec![paper("a", 300)]);
        let (m, st) = merge_bootstrap(&l, &r);
        assert_eq!(m.papers.len(), 2, "不能因为快照为空就删本地");
        assert_eq!(st.deleted, 0);
        assert_eq!(m.papers.iter().find(|p| p.id == "a").unwrap().gist, "rev300");
    }

    /// 删除保护按快照算「会消失几篇」，新增不计入
    #[test]
    fn 删除保护统计会消失的篇数() {
        let s = snap(&[("a", 1), ("b", 2), ("c", 3)]);
        assert_eq!(count_removals(&s, &lib(vec![paper("a", 1), paper("c", 3)])), 1);
        assert_eq!(count_removals(&s, &lib(vec![])), 3, "本地被清空时应数出全部");
        assert_eq!(
            count_removals(&s, &lib(vec![paper("a", 1), paper("b", 2), paper("c", 3), paper("d", 4)])),
            0,
            "新增不能算作删除"
        );
        assert_eq!(
            count_removals(&HashMap::new(), &lib(vec![])),
            0,
            "没有快照时不能触发保护，否则首次同步必然被拦"
        );
    }

    #[test]
    fn 上传前清空本机路径且不改动原库() {        let mut p = paper("a", 100);
        p.file = "/Users/me/files/a.pdf".into();
        let l = lib(vec![p]);
        let bytes = for_remote(&l);
        let sent: Library = serde_json::from_slice(&bytes).unwrap();
        assert!(sent.papers[0].file.is_empty());
        assert_eq!(l.papers[0].file, "/Users/me/files/a.pdf", "原库不能被改");
    }

    #[test]
    fn 远端带来的本机路径会被清掉() {
        let mut r = paper("b", 200);
        r.file = "/Users/someone-else/files/b.pdf".into();
        r.source = "/Users/someone-else/Downloads/b.pdf".into();
        let (m, _) = merge_bootstrap(&lib(vec![]), &lib(vec![r]));
        let b = m.papers.iter().find(|p| p.id == "b").unwrap();
        assert!(b.file.is_empty(), "另一台机器的绝对路径不能留");
        assert!(b.source.is_empty());
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
            let body = br#"{"papers":[]}"#.to_vec();
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
