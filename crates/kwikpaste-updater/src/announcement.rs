//! 应用内公告（从 1.x 的 `update/announcement.rs` 搬来）：随检查更新从官网后端拉取，由宿主弹对话框。
//!
//! 约定见官网仓库 `deploy/announcements.md`（schema 1，冻结、只读）。后端已经按版本、平台和
//! 系统筛选过，这里仍把每条规则重新判断一遍：CDN 快照是没筛选过的兜底来源，后台万一被攻破也不能把
//! 任意内容推到用户面前，所以只显示纯文本，链接只放行 https 的 fastthree.com / github.com / gitee.com。
//!
//! 每次启动最多弹一条（重要的优先），任何公告每天最多弹一次；状态记在 `<bootstrap>/announcements.json`
//! （与 1.x 共用，不进 `settings.json`，也就不会随备份带到别的电脑）。对话框由宿主显示：
//! 本模块给出 [`AnnouncementPrompt`]（标题、正文、按顺序排好的按钮），宿主返回 [`AnnouncementOutcome`]。

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, NaiveDate, Utc};
use kwikpaste_core::i18n::announcement::words;
use kwikpaste_core::settings::Language;
use kwikpaste_core::{AppEnv, Core};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::UpdaterUi;
use crate::os::OsVersion;

const ENDPOINT: &str = "https://paste.fastthree.com/api/v1/announcements";
/// 后端连不上时的兜底：同样格式、没按客户端筛选的 CDN 快照。
const SNAPSHOT: &str = "https://dl.fastthree.com/kwikpaste/announcements.json";
const EVENTS_PATH: &str = "/api/v1/announcement-events";
const STATE_FILENAME: &str = "announcements.json";
const STATE_VERSION: u16 = 1;
const SCHEMA: u64 = 1;
/// 启动时的拉取和随后的自动检查往往前后脚到，这个间隔内不重复请求。
const MIN_FETCH_INTERVAL: Duration = Duration::from_secs(60);
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const TITLE_MAX: usize = 40;
const BODY_MAX: usize = 300;
const ACTION_MAX: usize = 12;
const ID_MAX: usize = 40;
const ALLOWED_HOSTS: [&str; 3] = ["fastthree.com", "github.com", "gitee.com"];
/// 状态文件最多记住这么多条公告，超出时丢掉最早弹出的。
const MAX_REMEMBERED: usize = 200;
const PLATFORM: &str = if cfg!(target_os = "macos") {
    "macos"
} else if cfg!(target_os = "linux") {
    "linux"
} else {
    "windows"
};

#[derive(Default)]
pub(crate) struct AnnouncementState {
    gate: tokio::sync::Mutex<Option<Instant>>,
    /// 每次启动最多弹一条。
    shown_this_launch: AtomicBool,
}

/// 要显示的一条公告。按钮已按显示顺序排好、文案已按界面语言取好。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnouncementPrompt {
    pub id: String,
    /// `important` 级别。
    pub important: bool,
    /// 纯文本，单行。
    pub title: String,
    /// 纯文本，可以有换行。
    pub body: String,
    pub buttons: Vec<AnnouncementButton>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnouncementButton {
    pub role: ButtonRole,
    pub label: String,
}

/// 按钮的作用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonRole {
    /// 公告自己的按钮：打开链接。
    Action,
    /// 「不再提醒」。
    Dismiss,
    /// 「稍后」。
    Later,
    /// 「关闭」。
    Close,
    /// 「知道了」。
    GotIt,
}

impl ButtonRole {
    /// 点这个按钮的结果。
    pub fn outcome(self) -> AnnouncementOutcome {
        match self {
            Self::Action => AnnouncementOutcome::Clicked,
            Self::Dismiss => AnnouncementOutcome::Dismissed,
            Self::Later | Self::Close | Self::GotIt => AnnouncementOutcome::Closed,
        }
    }
}

/// 用户怎么响应的。按 Esc、点标题栏关闭都是 `Closed`，绝不能当成「不再提醒」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnouncementOutcome {
    /// 点了公告自己的按钮：更新器随后让宿主打开链接。
    Clicked,
    /// 点了「不再提醒」。
    Dismissed,
    /// 关掉（「稍后」「关闭」「知道了」、Esc 和标题栏关闭）。
    Closed,
}

/// 在 core runtime 后台拉取并按需交给宿主显示，不阻塞检查更新；失败只记调试日志。
pub(crate) fn schedule(core: &Core, state: &Arc<AnnouncementState>, ui: &Arc<dyn UpdaterUi>) {
    let Some(sources) = sources(core.info().env) else {
        return;
    };

    let core = core.clone();
    let state = state.clone();
    let ui = ui.clone();
    core.runtime().clone().spawn(async move {
        let mut last_fetch = state.gate.lock().await;
        if state.shown_this_launch.load(Ordering::Relaxed)
            || last_fetch.is_some_and(|at| at.elapsed() < MIN_FETCH_INTERVAL)
        {
            return;
        }
        *last_fetch = Some(Instant::now());
        if let Err(err) = run(&core, &state, ui.as_ref(), &sources).await {
            log::debug!("announcement check skipped: {err:#}");
        }
    });
}

pub(crate) struct Sources {
    api: Url,
    snapshot: Option<Url>,
}

/// 开发构建默认不拉，避免本地调试混进线上统计；`e2e-overrides` 下可以指定接口地址（只读它，不读快照）。
fn sources(env: AppEnv) -> Option<Sources> {
    if let Some(endpoint) = crate::overrides::var(crate::overrides::ANNOUNCEMENT_ENDPOINT) {
        return Url::parse(&endpoint).ok().map(|api| Sources {
            api,
            snapshot: None,
        });
    }
    (env == AppEnv::Prod).then(|| Sources {
        api: Url::parse(ENDPOINT).expect("valid announcement endpoint"),
        snapshot: Url::parse(SNAPSHOT).ok(),
    })
}

async fn run(
    core: &Core,
    state: &AnnouncementState,
    ui: &dyn UpdaterUi,
    sources: &Sources,
) -> Result<()> {
    let lang = core.settings().appearance.language;
    let facts = ClientFacts {
        app_version: core.info().version.clone(),
        os: crate::os::current(),
    };
    let items = match fetch(&request_url(&sources.api, &facts, lang)).await {
        Ok(items) => items,
        Err(err) => {
            let Some(snapshot) = &sources.snapshot else {
                return Err(err);
            };
            log::debug!("announcement api unavailable, trying snapshot: {err:#}");
            fetch(snapshot).await?
        }
    };

    let now = Utc::now();
    let today = Local::now().date_naive();
    let path = core.paths().bootstrap_dir().join(STATE_FILENAME);
    let mut remembered = load_state(&path);
    let candidates: Vec<Announcement> = items
        .into_iter()
        .filter(|item| item.matches(&facts, now))
        .collect();
    let Some(item) = pick(&candidates, &remembered, today) else {
        return Ok(());
    };

    // 弹出前就记下来：对话框开着时应用崩溃或被关掉，重启后也不会重复弹。
    state.shown_this_launch.store(true, Ordering::Relaxed);
    remembered.mark_shown(&item.id, today);
    write_state(&path, &remembered)?;
    send_event(&sources.api, &item.id, "shown");

    match ui.show_announcement(prompt(item, lang)).await {
        AnnouncementOutcome::Clicked => {
            send_event(&sources.api, &item.id, "clicked");
            if let Some(action) = &item.action {
                ui.open_url(action.url.as_str());
            }
        }
        AnnouncementOutcome::Dismissed => {
            send_event(&sources.api, &item.id, "dismissed");
            remembered.dismiss(&item.id);
            write_state(&path, &remembered)?;
        }
        AnnouncementOutcome::Closed => {}
    }

    Ok(())
}

/* ---------- 客户端事实 ---------- */

struct ClientFacts {
    app_version: semver::Version,
    os: Option<OsVersion>,
}

fn request_url(api: &Url, facts: &ClientFacts, lang: Language) -> Url {
    let mut url = api.clone();
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("app_version", &facts.app_version.to_string());
        query.append_pair("platform", PLATFORM);
        if let Some(os) = facts.os {
            query.append_pair("os", &os.query_value());
        }
        query.append_pair(
            "locale",
            match lang {
                Language::ZhCN => "zh-CN",
                Language::EnUS => "en-US",
            },
        );
    }
    url
}

/* ---------- 拉取与校验 ---------- */

/// 独立的请求客户端，不跟随重定向，响应大小有上限。
async fn fetch(url: &Url) -> Result<Vec<Announcement>> {
    let response = crate::http::no_redirect_client()?
        .get(url.clone())
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        bail!("announcement source answered {status}");
    }
    let bytes = crate::http::read_limited(response, MAX_RESPONSE_BYTES).await?;
    parse(&bytes)
}

#[derive(Deserialize)]
struct Envelope {
    schema: u64,
    #[serde(default)]
    items: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawItem {
    id: String,
    level: String,
    title: Localized,
    body: Localized,
    action: Option<RawAction>,
    #[serde(rename = "match")]
    rules: RawMatch,
    starts_at: Option<String>,
    ends_at: Option<String>,
    repeat: String,
    #[serde(default)]
    updated_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct Localized {
    #[serde(rename = "zh-CN")]
    zh: String,
    #[serde(rename = "en-US")]
    en: String,
}

impl Localized {
    fn get(&self, lang: Language) -> &str {
        match lang {
            Language::ZhCN => &self.zh,
            Language::EnUS => &self.en,
        }
    }
}

#[derive(Deserialize)]
struct RawAction {
    label: Localized,
    url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawMatch {
    platforms: Vec<String>,
    min_app_version: Option<String>,
    max_app_version: Option<String>,
    min_windows_build: Option<u64>,
    min_macos: Option<String>,
}

#[derive(Clone, Debug)]
struct Action {
    label: Localized,
    url: Url,
}

#[derive(Clone, Debug)]
struct Announcement {
    id: String,
    important: bool,
    daily: bool,
    title: Localized,
    body: Localized,
    action: Option<Action>,
    platforms: Vec<String>,
    min_app_version: Option<semver::Version>,
    max_app_version: Option<semver::Version>,
    min_windows_build: Option<u64>,
    min_macos: Option<(u64, u64, u64)>,
    starts_at: Option<DateTime<Utc>>,
    ends_at: Option<DateTime<Utc>>,
    updated_at: Option<DateTime<Utc>>,
}

/// 格式版本不认识就整份忽略；单条结构或内容不合规只跳过那一条，不认识的字段直接忽略。
fn parse(bytes: &[u8]) -> Result<Vec<Announcement>> {
    let envelope: Envelope = serde_json::from_slice(bytes).context("parse announcements")?;
    if envelope.schema != SCHEMA {
        return Ok(Vec::new());
    }
    let mut items = Vec::new();
    for value in envelope.items {
        let validated = serde_json::from_value::<RawItem>(value)
            .context("unexpected announcement shape")
            .and_then(validate);
        match validated {
            Ok(item) => items.push(item),
            Err(err) => log::debug!("skip announcement: {err:#}"),
        }
    }
    Ok(items)
}

fn validate(raw: RawItem) -> Result<Announcement> {
    if !valid_id(&raw.id) {
        bail!("invalid id");
    }
    let important = match raw.level.as_str() {
        "info" => false,
        "important" => true,
        _ => bail!("unknown level"),
    };
    let daily = match raw.repeat.as_str() {
        "once" => false,
        "daily" => true,
        _ => bail!("unknown repeat"),
    };
    check_localized(&raw.title, TITLE_MAX, false)?;
    check_localized(&raw.body, BODY_MAX, true)?;
    let action = match raw.action {
        Some(action) => {
            check_localized(&action.label, ACTION_MAX, false)?;
            let url = allowed_url(&action.url).context("link is not allowed")?;
            Some(Action {
                label: action.label,
                url,
            })
        }
        None => None,
    };
    if raw.rules.platforms.is_empty() {
        bail!("no platforms");
    }
    Ok(Announcement {
        id: raw.id,
        important,
        daily,
        title: raw.title,
        body: raw.body,
        action,
        platforms: raw.rules.platforms,
        min_app_version: raw
            .rules
            .min_app_version
            .as_deref()
            .map(parse_app_version)
            .transpose()?,
        max_app_version: raw
            .rules
            .max_app_version
            .as_deref()
            .map(parse_app_version)
            .transpose()?,
        min_windows_build: raw.rules.min_windows_build,
        min_macos: raw
            .rules
            .min_macos
            .as_deref()
            .map(|value| crate::os::parse_macos_version(value).context("invalid macOS version"))
            .transpose()?,
        starts_at: raw.starts_at.as_deref().map(parse_time).transpose()?,
        ends_at: raw.ends_at.as_deref().map(parse_time).transpose()?,
        updated_at: raw
            .updated_at
            .as_deref()
            .and_then(|value| parse_time(value).ok()),
    })
}

fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    id.len() <= ID_MAX
        && chars
            .next()
            .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn check_localized(text: &Localized, max: usize, multiline: bool) -> Result<()> {
    check_text(&text.zh, max, multiline)?;
    check_text(&text.en, max, multiline)
}

/// 纯文本：去掉首尾空白后不能为空、不超长，除正文里的换行外不能有控制字符。
fn check_text(text: &str, max: usize, multiline: bool) -> Result<()> {
    let text = text.trim();
    if text.is_empty() || text.chars().count() > max {
        bail!("text is empty or too long");
    }
    if text
        .chars()
        .any(|c| c.is_control() && !(multiline && c == '\n'))
    {
        bail!("text contains control characters");
    }
    Ok(())
}

/// https、默认端口、不带用户名密码，域名是白名单里的或它们的子域名。
fn allowed_url(value: &str) -> Option<Url> {
    let url = Url::parse(value).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    let allowed = url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && ALLOWED_HOSTS
            .iter()
            .any(|allowed| host == *allowed || host.ends_with(&format!(".{allowed}")));
    allowed.then_some(url)
}

/// 版本比较按 semver 规则：预发布版低于同号正式版，构建元数据不参与比较。
fn parse_app_version(value: &str) -> Result<semver::Version> {
    let mut version = semver::Version::parse(value).context("invalid app version")?;
    version.build = semver::BuildMetadata::EMPTY;
    Ok(version)
}

fn parse_time(value: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)
        .context("invalid timestamp")?
        .with_timezone(&Utc))
}

impl Announcement {
    /// 与后端相同的规则；客户端拿不到的条件（读不出系统版本）按不满足处理，宁可不弹。
    fn matches(&self, facts: &ClientFacts, now: DateTime<Utc>) -> bool {
        if !self.platforms.iter().any(|platform| platform == PLATFORM) {
            return false;
        }
        if self.starts_at.is_some_and(|starts| starts > now)
            || self.ends_at.is_some_and(|ends| ends <= now)
        {
            return false;
        }
        let mut current = facts.app_version.clone();
        current.build = semver::BuildMetadata::EMPTY;
        if self
            .min_app_version
            .as_ref()
            .is_some_and(|min| current < *min)
            || self
                .max_app_version
                .as_ref()
                .is_some_and(|max| current > *max)
        {
            return false;
        }
        if PLATFORM == "windows" {
            if let Some(min) = self.min_windows_build {
                return matches!(facts.os, Some(OsVersion::WindowsBuild(build)) if build >= min);
            }
        } else if PLATFORM == "macos"
            && let Some(min) = self.min_macos
        {
            return matches!(facts.os, Some(OsVersion::Macos(version)) if version >= min);
        }
        true
    }
}

/* ---------- 挑选与记忆 ---------- */

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Remembered {
    version: u16,
    #[serde(default)]
    dismissed: Vec<String>,
    /// 每条公告最后一次弹出的本地日期。
    #[serde(default)]
    shown: BTreeMap<String, NaiveDate>,
    #[serde(default)]
    last_shown_day: Option<NaiveDate>,
}

impl Remembered {
    fn mark_shown(&mut self, id: &str, today: NaiveDate) {
        self.shown.insert(id.to_owned(), today);
        self.last_shown_day = Some(today);
        while self.shown.len() > MAX_REMEMBERED {
            let oldest = self
                .shown
                .iter()
                .min_by_key(|(_, day)| **day)
                .map(|(id, _)| id.clone());
            match oldest {
                Some(id) => self.shown.remove(&id),
                None => break,
            };
        }
    }

    fn dismiss(&mut self, id: &str) {
        if !self.dismissed.iter().any(|known| known == id) {
            self.dismissed.push(id.to_owned());
        }
        let overflow = self.dismissed.len().saturating_sub(MAX_REMEMBERED);
        self.dismissed.drain(..overflow);
    }
}

/// 一天最多弹一次；重要的优先，其余按更新时间从新到旧。
/// 只提醒一次的弹过就不再弹，每天提醒的直到点「不再提醒」。
fn pick<'a>(
    items: &'a [Announcement],
    remembered: &Remembered,
    today: NaiveDate,
) -> Option<&'a Announcement> {
    if remembered.last_shown_day == Some(today) {
        return None;
    }
    items
        .iter()
        .filter(|item| !remembered.dismissed.contains(&item.id))
        .filter(|item| match remembered.shown.get(&item.id) {
            Some(day) => item.daily && *day != today,
            None => true,
        })
        .max_by(|a, b| {
            (a.important, a.updated_at)
                .cmp(&(b.important, b.updated_at))
                .then_with(|| b.id.cmp(&a.id))
        })
}

/// 文件缺失、损坏或格式版本不对都当作什么都没记住。
fn load_state(path: &Path) -> Remembered {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Remembered>(&bytes).ok())
        .filter(|state| state.version == STATE_VERSION)
        .unwrap_or(Remembered {
            version: STATE_VERSION,
            ..Remembered::default()
        })
}

/// 放在启动锚点目录：不进 `settings.json`，也就不会随备份带到别的电脑上。
fn write_state(path: &Path, state: &Remembered) -> Result<()> {
    let parent = path
        .parent()
        .context("announcement state has no parent directory")?;
    fs::create_dir_all(parent).context("create announcement state directory")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(&serde_json::to_vec(state)?)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).context("save announcement state")?;
    Ok(())
}

/* ---------- 对话框 ---------- */

/// 按钮排布保证按 Esc 或点标题栏关闭永远落在「稍后 / 关闭」，不会被当成「不再提醒」：
///
/// | `repeat` | 有链接 | 没链接 |
/// |---|---|---|
/// | `once` | [按钮文字] [关闭] | [知道了] |
/// | `daily` | [按钮文字] [不再提醒] [稍后] | [不再提醒] [稍后] |
fn prompt(item: &Announcement, lang: Language) -> AnnouncementPrompt {
    let words = words(lang);
    let button = |role, label: &str| AnnouncementButton {
        role,
        label: label.trim().to_owned(),
    };
    let action = item
        .action
        .as_ref()
        .map(|action| button(ButtonRole::Action, action.label.get(lang)));
    let buttons = match (item.daily, action) {
        (false, Some(action)) => vec![action, button(ButtonRole::Close, words.close)],
        (false, None) => vec![button(ButtonRole::GotIt, words.got_it)],
        (true, Some(action)) => vec![
            action,
            button(ButtonRole::Dismiss, words.dismiss),
            button(ButtonRole::Later, words.later),
        ],
        (true, None) => vec![
            button(ButtonRole::Dismiss, words.dismiss),
            button(ButtonRole::Later, words.later),
        ],
    };

    AnnouncementPrompt {
        id: item.id.clone(),
        important: item.important,
        title: item.title.get(lang).trim().to_owned(),
        body: item.body.get(lang).trim().to_owned(),
        buttons,
    }
}

/// 只带公告 id 和事件类型；发送失败不重试，也不影响用户操作。
fn send_event(api: &Url, id: &str, event: &'static str) {
    let Ok(url) = api.join(EVENTS_PATH) else {
        return;
    };
    let body = serde_json::json!({ "id": id, "event": event });
    tokio::spawn(async move {
        let sent = async {
            crate::http::no_redirect_client()?
                .post(url)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(serde_json::to_vec(&body)?)
                .send()
                .await?;
            anyhow::Ok(())
        };
        if let Err(err) = sent.await {
            log::debug!("announcement event skipped: {err:#}");
        }
    });
}

#[cfg(test)]
mod tests;
