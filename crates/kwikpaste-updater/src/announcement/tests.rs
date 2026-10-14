//! 公告的解析、筛选、挑选与记忆（从 1.x 移植），对话框按钮排布，以及经本地 HTTP 服务的完整流程。

use std::sync::Mutex;
use std::time::Duration;

use super::*;
use crate::handoff::HostFuture;
use crate::http::testing::{Reply, Server};
use crate::testing::TestCore;

fn at(time: &str) -> DateTime<Utc> {
    parse_time(time).unwrap()
}

fn day(value: &str) -> NaiveDate {
    NaiveDate::parse_from_str(value, "%Y-%m-%d").unwrap()
}

fn item_json(id: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "level": "info",
        "title": { "zh-CN": "标题", "en-US": "Title" },
        "body": { "zh-CN": "第一行\n第二行", "en-US": "Body" },
        "action": { "label": { "zh-CN": "去下载", "en-US": "Download" }, "url": "https://paste.fastthree.com/" },
        "match": { "platforms": ["windows", "macos", "linux"], "minAppVersion": null, "maxAppVersion": null, "minWindowsBuild": null, "minMacos": null },
        "startsAt": null,
        "endsAt": null,
        "repeat": "once",
        "updatedAt": "2026-10-02T07:00:00Z",
        "futureField": { "ignored": true }
    })
}

fn items(values: Vec<serde_json::Value>) -> Vec<Announcement> {
    parse(&serde_json::to_vec(&serde_json::json!({ "schema": 1, "items": values })).unwrap())
        .unwrap()
}

fn one(mutate: impl FnOnce(&mut serde_json::Value)) -> Vec<Announcement> {
    let mut value = item_json("a");
    mutate(&mut value);
    items(vec![value])
}

fn facts(version: &str, os: Option<OsVersion>) -> ClientFacts {
    ClientFacts {
        app_version: semver::Version::parse(version).unwrap(),
        os,
    }
}

#[test]
fn unknown_schema_is_ignored_and_bad_items_are_skipped() {
    let bytes =
        serde_json::to_vec(&serde_json::json!({ "schema": 2, "items": [item_json("a")] })).unwrap();
    assert!(parse(&bytes).unwrap().is_empty());
    let mut broken = item_json("broken");
    broken["title"] = serde_json::json!("not localized");
    let parsed = items(vec![item_json("good"), broken, serde_json::json!(42)]);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].id, "good");
    assert!(parse(b"not json").is_err());
}

#[test]
fn content_rules_match_the_server() {
    assert_eq!(one(|_| {}).len(), 1);
    for mutate in [
        |v: &mut serde_json::Value| v["id"] = "Bad Id".into(),
        |v: &mut serde_json::Value| v["level"] = "urgent".into(),
        |v: &mut serde_json::Value| v["repeat"] = "weekly".into(),
        |v: &mut serde_json::Value| v["title"]["zh-CN"] = "长".repeat(41).into(),
        |v: &mut serde_json::Value| v["title"]["en-US"] = "tab\there".into(),
        |v: &mut serde_json::Value| v["title"]["zh-CN"] = "  ".into(),
        |v: &mut serde_json::Value| v["body"]["en-US"] = "x".repeat(301).into(),
        |v: &mut serde_json::Value| v["body"]["zh-CN"] = "line\r\nbreak".into(),
        |v: &mut serde_json::Value| {
            v["action"]["label"]["zh-CN"] = "十三个字的按钮文字太长了吧".into()
        },
        |v: &mut serde_json::Value| v["match"]["platforms"] = serde_json::json!([]),
        |v: &mut serde_json::Value| v["match"]["minAppVersion"] = "v2".into(),
        |v: &mut serde_json::Value| v["match"]["minMacos"] = "ten".into(),
        |v: &mut serde_json::Value| v["startsAt"] = "tomorrow".into(),
    ] {
        assert!(one(mutate).is_empty());
    }
}

#[test]
fn links_are_limited_to_https_on_our_hosts() {
    for ok in [
        "https://paste.fastthree.com/",
        "https://fastthree.com/x",
        "https://github.com/ManSanDADADA/KwikPaste/releases",
        "https://gitee.com/mansan33",
        "https://paste.fastthree.com:443/",
    ] {
        assert!(allowed_url(ok).is_some(), "{ok}");
    }
    for bad in [
        "http://paste.fastthree.com/",
        "https://evil.example/",
        "https://fastthree.com.evil.example/",
        "https://evilfastthree.com/",
        "https://user:pw@paste.fastthree.com/",
        "https://paste.fastthree.com:8443/",
        "javascript:alert(1)",
        "file:///C:/Windows",
    ] {
        assert!(allowed_url(bad).is_none(), "{bad}");
    }
    assert!(one(|v| v["action"]["url"] = "https://evil.example/".into()).is_empty());
    let plain = one(|v| v["action"] = serde_json::Value::Null);
    assert!(plain[0].action.is_none());
}

#[test]
fn audience_rules() {
    let now = at("2026-10-02T08:00:00Z");
    let os = if PLATFORM == "windows" {
        Some(OsVersion::WindowsBuild(26100))
    } else {
        Some(OsVersion::Macos((14, 6, 1)))
    };
    let old_os = if PLATFORM == "windows" {
        Some(OsVersion::WindowsBuild(14393))
    } else {
        Some(OsVersion::Macos((10, 14, 0)))
    };
    let native = one(|v| {
        v["match"]["maxAppVersion"] = "1.99.99".into();
        v["match"]["minWindowsBuild"] = 17134.into();
        v["match"]["minMacos"] = "10.15".into();
    });
    assert!(native[0].matches(&facts("1.4.0", os), now));
    assert!(native[0].matches(&facts("1.4.0-beta.1", os), now));
    assert!(!native[0].matches(&facts("2.0.0", os), now));
    // Windows/macOS 的最低系统版本不约束 Linux；平台名单仍必须显式匹配。
    assert_eq!(
        native[0].matches(&facts("1.4.0", old_os), now),
        PLATFORM == "linux"
    );
    assert_eq!(
        native[0].matches(&facts("1.4.0", None), now),
        PLATFORM == "linux"
    );

    let fresh = one(|v| v["match"]["minAppVersion"] = "1.4.0".into());
    assert!(!fresh[0].matches(&facts("1.4.0-beta.1", os), now));
    assert!(fresh[0].matches(&facts("1.4.0+build.7", os), now));

    let mac_only = one(|v| v["match"]["platforms"] = serde_json::json!(["macos"]));
    assert_eq!(
        mac_only[0].matches(&facts("1.4.0", os), now),
        PLATFORM == "macos"
    );

    let later = one(|v| v["startsAt"] = "2026-10-03T00:00:00Z".into());
    assert!(!later[0].matches(&facts("1.4.0", os), now));
    let over = one(|v| v["endsAt"] = "2026-10-02T08:00:00Z".into());
    assert!(!over[0].matches(&facts("1.4.0", os), now));
}

#[test]
fn picking_follows_repeat_dismissal_and_daily_limit() {
    let mut once = item_json("once");
    once["level"] = "important".into();
    let mut daily = item_json("daily");
    daily["repeat"] = "daily".into();
    let list = items(vec![daily, once]);
    let today = day("2026-10-02");
    let mut remembered = Remembered::default();

    assert_eq!(pick(&list, &remembered, today).unwrap().id, "once");
    remembered.mark_shown("once", today);
    assert!(pick(&list, &remembered, today).is_none());

    let tomorrow = day("2026-10-03");
    assert_eq!(pick(&list, &remembered, tomorrow).unwrap().id, "daily");
    remembered.mark_shown("daily", tomorrow);
    let after = day("2026-10-04");
    assert_eq!(pick(&list, &remembered, after).unwrap().id, "daily");
    remembered.dismiss("daily");
    assert!(pick(&list, &remembered, after).is_none());
}

#[test]
fn newer_announcement_wins_within_the_same_level() {
    let mut old = item_json("old");
    old["updatedAt"] = "2026-10-01T00:00:00Z".into();
    let list = items(vec![old, item_json("new")]);
    assert_eq!(
        pick(&list, &Remembered::default(), day("2026-10-02"))
            .unwrap()
            .id,
        "new"
    );
}

#[test]
fn remembered_state_survives_and_stays_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(STATE_FILENAME);
    assert_eq!(load_state(&path).version, STATE_VERSION);
    let mut state = load_state(&path);
    for index in 0..(MAX_REMEMBERED + 5) {
        let id = format!("a{index}");
        state.mark_shown(&id, day("2026-01-01") + chrono::Days::new(index as u64));
        state.dismiss(&id);
    }
    write_state(&path, &state).unwrap();
    let loaded = load_state(&path);
    assert_eq!(loaded.shown.len(), MAX_REMEMBERED);
    assert_eq!(loaded.dismissed.len(), MAX_REMEMBERED);
    assert!(!loaded.shown.contains_key("a0"));
    assert!(!loaded.dismissed.iter().any(|id| id == "a0"));
    fs::write(&path, b"{ broken").unwrap();
    assert!(load_state(&path).shown.is_empty());
}

/// 与 1.x 的四种排布一致；只有「不再提醒」按钮算 `Dismissed`，Esc / 关闭永远是 `Closed`。
#[test]
fn prompt_buttons_follow_the_frozen_layout() {
    let roles = |value: serde_json::Value| -> Vec<ButtonRole> {
        let list = items(vec![value]);
        prompt(&list[0], Language::ZhCN)
            .buttons
            .iter()
            .map(|button| button.role)
            .collect()
    };
    let mut daily = item_json("daily");
    daily["repeat"] = "daily".into();
    let mut daily_plain = daily.clone();
    daily_plain["action"] = serde_json::Value::Null;
    let mut once_plain = item_json("plain");
    once_plain["action"] = serde_json::Value::Null;

    assert_eq!(
        roles(item_json("once")),
        [ButtonRole::Action, ButtonRole::Close]
    );
    assert_eq!(roles(once_plain), [ButtonRole::GotIt]);
    assert_eq!(
        roles(daily),
        [ButtonRole::Action, ButtonRole::Dismiss, ButtonRole::Later]
    );
    assert_eq!(roles(daily_plain), [ButtonRole::Dismiss, ButtonRole::Later]);

    assert_eq!(ButtonRole::Action.outcome(), AnnouncementOutcome::Clicked);
    assert_eq!(
        ButtonRole::Dismiss.outcome(),
        AnnouncementOutcome::Dismissed
    );
    for role in [ButtonRole::Later, ButtonRole::Close, ButtonRole::GotIt] {
        assert_eq!(role.outcome(), AnnouncementOutcome::Closed);
    }

    let list = items(vec![item_json("text")]);
    let zh = prompt(&list[0], Language::ZhCN);
    assert_eq!(zh.title, "标题");
    assert_eq!(zh.body, "第一行\n第二行");
    assert_eq!(zh.buttons[0].label, "去下载");
    let en = prompt(&list[0], Language::EnUS);
    assert_eq!(en.title, "Title");
    assert_eq!(en.buttons[0].label, "Download");
}

#[test]
fn request_carries_only_the_documented_facts() {
    let api = Url::parse(ENDPOINT).unwrap();
    let url = request_url(
        &api,
        &facts("1.4.0", Some(OsVersion::WindowsBuild(26100))),
        Language::ZhCN,
    );
    let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    let keys: Vec<&str> = pairs.iter().map(|(key, _)| key.as_str()).collect();
    assert_eq!(keys, ["app_version", "platform", "os", "locale"]);
    assert!(pairs.contains(&("os".into(), "26100".into())));
}

#[test]
fn development_builds_do_not_fetch_by_default() {
    if cfg!(feature = "e2e-overrides") {
        return;
    }
    assert!(sources(AppEnv::Dev).is_none());
    let prod = sources(AppEnv::Prod).unwrap();
    assert_eq!(prod.api.as_str(), ENDPOINT);
    assert_eq!(prod.snapshot.unwrap().as_str(), SNAPSHOT);
}

#[test]
fn fetch_accepts_ok_json_and_rejects_errors_and_oversized_bodies() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let ok =
        serde_json::to_vec(&serde_json::json!({ "schema": 1, "items": [item_json("a")] })).unwrap();
    let fetch_from = |reply: Reply| {
        let server = Server::start(vec![reply]);
        let url = Url::parse(&server.url("/api/v1/announcements")).unwrap();
        runtime.block_on(fetch(&url))
    };

    assert_eq!(fetch_from(Reply::ok(ok)).unwrap().len(), 1);
    assert!(fetch_from(Reply::status("500 Internal Server Error")).is_err());
    assert!(
        fetch_from(Reply::status("302 Found").header("Location", "https://evil.example/")).is_err()
    );
    assert!(fetch_from(Reply::ok(vec![b' '; MAX_RESPONSE_BYTES + 1])).is_err());
}

/// 宿主的假对话框：记下收到的公告，按预设返回用户的响应。
struct FakeUi {
    outcome: AnnouncementOutcome,
    prompts: Mutex<Vec<AnnouncementPrompt>>,
    opened: Mutex<Vec<String>>,
}

impl UpdaterUi for FakeUi {
    fn update_available(&self, _status: crate::UpdateStatus) {}

    fn show_announcement(&self, prompt: AnnouncementPrompt) -> HostFuture<'_, AnnouncementOutcome> {
        self.prompts.lock().unwrap().push(prompt);
        let outcome = self.outcome;
        Box::pin(async move { outcome })
    }

    fn open_url(&self, url: &str) {
        self.opened.lock().unwrap().push(url.to_owned());
    }
}

fn fake_ui(outcome: AnnouncementOutcome) -> FakeUi {
    FakeUi {
        outcome,
        prompts: Mutex::default(),
        opened: Mutex::default(),
    }
}

fn wait_for_requests(server: &Server, count: usize) -> Vec<crate::http::testing::Request> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let requests = server.requests();
        if requests.len() >= count || Instant::now() > deadline {
            return requests;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// 完整流程：接口 → 筛选 → 弹出前记状态并上报 shown → 宿主响应 → 上报 clicked、打开链接。
#[test]
fn clicked_announcement_reports_events_and_opens_the_link() {
    let test = TestCore::start("2.0.0");
    let body =
        serde_json::to_vec(&serde_json::json!({ "schema": 1, "items": [item_json("native-2-0")] }))
            .unwrap();
    let server = Server::start(vec![
        Reply::ok(body),
        Reply::status("204 No Content"),
        Reply::status("204 No Content"),
    ]);
    let sources = Sources {
        api: Url::parse(&server.url("/api/v1/announcements")).unwrap(),
        snapshot: None,
    };
    let state = AnnouncementState::default();
    let ui = fake_ui(AnnouncementOutcome::Clicked);

    test.block_on(run(&test.core, &state, &ui, &sources))
        .unwrap();

    let prompts = ui.prompts.lock().unwrap().clone();
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].id, "native-2-0");
    assert_eq!(*ui.opened.lock().unwrap(), ["https://paste.fastthree.com/"]);
    let requests = wait_for_requests(&server, 3);
    assert!(
        requests[0]
            .line
            .starts_with("GET /api/v1/announcements?app_version=2.0.0&platform=")
    );
    let mut events: Vec<String> = requests[1..]
        .iter()
        .map(|request| {
            assert!(request.line.starts_with("POST /api/v1/announcement-events"));
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(body["id"], "native-2-0");
            body["event"].as_str().unwrap().to_owned()
        })
        .collect();
    events.sort();
    assert_eq!(events, ["clicked", "shown"]);

    let saved = load_state(&test.core.paths().bootstrap_dir().join(STATE_FILENAME));
    assert!(saved.shown.contains_key("native-2-0"));
    assert!(saved.dismissed.is_empty());
    assert!(state.shown_this_launch.load(Ordering::Relaxed));
}

/// 接口不通读快照；点「不再提醒」记进状态文件。
#[test]
fn snapshot_fallback_and_dismissal_are_remembered() {
    let test = TestCore::start("2.0.0");
    let mut daily = item_json("daily-tip");
    daily["repeat"] = "daily".into();
    let snapshot =
        serde_json::to_vec(&serde_json::json!({ "schema": 1, "items": [daily] })).unwrap();
    let api = Server::start(vec![
        Reply::status("503 Service Unavailable"),
        Reply::status("204 No Content"),
        Reply::status("204 No Content"),
    ]);
    let cdn = Server::start(vec![Reply::ok(snapshot)]);
    let sources = Sources {
        api: Url::parse(&api.url("/api/v1/announcements")).unwrap(),
        snapshot: Some(Url::parse(&cdn.url("/kwikpaste/announcements.json")).unwrap()),
    };
    let ui = fake_ui(AnnouncementOutcome::Dismissed);

    test.block_on(run(
        &test.core,
        &AnnouncementState::default(),
        &ui,
        &sources,
    ))
    .unwrap();

    assert_eq!(ui.prompts.lock().unwrap().len(), 1);
    assert!(ui.opened.lock().unwrap().is_empty());
    let saved = load_state(&test.core.paths().bootstrap_dir().join(STATE_FILENAME));
    assert_eq!(saved.dismissed, ["daily-tip"]);
    assert_eq!(wait_for_requests(&api, 3).len(), 3);
}
