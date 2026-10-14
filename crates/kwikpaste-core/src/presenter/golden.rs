//! 展示层 golden：用一组覆盖各类卡片的记录跑列表与预览加工，与 `tests/fixtures/presenter/` 比对。
//!
//! 夹具按 1.4.0 的加工规则录制，之后任何改动都必须让输出保持不变。设置环境变量
//! `KWIKPASTE_UPDATE_GOLDEN=1` 运行时重写夹具（只有确认行为应当改变时才这么做）。
//!
//! 比较前做三处与平台无关的归一化：临时数据目录换成 `<root>`、其下路径的分隔符换成 `/`，
//! macOS 的 `revealInFinder` 换成 Windows 的 `revealInExplorer`。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, FixedOffset, TimeZone, Utc};
use serde_json::Value;
use sqlx::SqlitePool;

use super::{build_preview_payload, present_list_item, ListContext};
use crate::clipboard::{AppIconStore, FileIconStore, ImageStore};
use crate::db::items::{content_hash, find_item_by_id, find_item_for_list_by_id, insert_item};
use crate::db::models::{
    ClipboardApp, ClipboardGroup, ClipboardItem, ClipboardItemQuery, ClipboardKind,
    ClipboardSubKind, Platform,
};
use crate::db::test_support::memory_pool;
use crate::presenter::ClipboardItemPage;
use crate::settings::Clipboard;

struct Scene {
    _temp: tempfile::TempDir,
    root: PathBuf,
    pool: SqlitePool,
    images: ImageStore,
    app_icons: AppIconStore,
    file_icons: FileIconStore,
}

fn tz() -> FixedOffset {
    FixedOffset::east_opt(8 * 3600).unwrap()
}

fn now() -> DateTime<FixedOffset> {
    tz().with_ymd_and_hms(2026, 10, 2, 15, 30, 0).unwrap()
}

fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
    tz().with_ymd_and_hms(y, mo, d, h, mi, 0)
        .unwrap()
        .with_timezone(&Utc)
}

fn current_platform() -> Platform {
    if cfg!(target_os = "macos") {
        Platform::Macos
    } else if cfg!(target_os = "linux") {
        Platform::Linux
    } else {
        Platform::Windows
    }
}

fn row(id: &str, kind: ClipboardKind, content: &str, created_at: DateTime<Utc>) -> ClipboardItem {
    let mut item = super::tests::text_item(None, false);
    item.id = id.to_owned();
    item.kind = kind;
    item.content = content.to_owned();
    item.content_hash = content_hash(kind, content);
    item.platform = Platform::Windows;
    item.created_at = created_at;
    item.updated_at = created_at;
    if kind == ClipboardKind::Text {
        item.search_text = Some(content.to_owned());
        item.summary = Some(content.to_owned());
        item.size = Some(content.len() as i64);
    }
    item
}

/// 建一组覆盖各类卡片的记录：纯文本带快捷信息、HTML、RTF、链接、邮箱、颜色、路径、
/// 敏感内容（含敏感颜色）、有无缩略图的图片、多文件、单个图片文件、同步来的记录、分组内记录。
async fn scene() -> Scene {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let pool = memory_pool().await;
    let resources = root.join("resources");
    let images = ImageStore::for_test(resources.join("clipboard-images"));
    let app_icons = AppIconStore::for_test(resources.join("app-icons"));
    let file_icons = FileIconStore::for_test(resources.join("file-icons"));

    let icon_file = app_icons.store(b"golden-app-icon").unwrap();
    crate::db::apps::upsert_app(
        &pool,
        &ClipboardApp {
            id: "com.example.editor".to_owned(),
            name: "Editor".to_owned(),
            icon_file: Some(icon_file),
            platform: Platform::Windows,
            created_at: at(2026, 1, 1, 9, 0),
            updated_at: at(2026, 1, 1, 9, 0),
        },
    )
    .await
    .unwrap();
    crate::db::groups::insert_group(
        &pool,
        &ClipboardGroup {
            id: "group-work".to_owned(),
            name: "工作".to_owned(),
            icon: "i-lets-icons:folder".to_owned(),
            is_hidden: false,
            sort_order: 0,
            created_at: at(2026, 1, 1, 9, 0),
            updated_at: at(2026, 1, 1, 9, 0),
        },
    )
    .await
    .unwrap();

    let png_file_icon = file_icons.store(b"golden-png-icon").unwrap();
    crate::db::file_icons::upsert_icon(&pool, ".png", current_platform(), &png_file_icon)
        .await
        .unwrap();

    let mut items = Vec::new();

    let mut plain = row(
        "text-plain",
        ClipboardKind::Text,
        "订单 20260924 已发货，联系 138 1234 5678",
        at(2026, 10, 2, 15, 0),
    );
    plain.is_pinned = true;
    plain.source_app_id = Some("com.example.editor".to_owned());
    items.push(plain);

    let mut html = row(
        "text-html",
        ClipboardKind::Text,
        "<b>Hello</b> World",
        at(2026, 10, 2, 14, 0),
    );
    html.sub_kind = Some(ClipboardSubKind::Html);
    html.search_text = Some("Hello World".to_owned());
    html.summary = Some("Hello World".to_owned());
    html.is_favorite = true;
    html.note = Some("问候语".to_owned());
    items.push(html);

    let mut rtf = row(
        "text-rtf",
        ClipboardKind::Text,
        r"{\rtf1 Plain RTF v1.3.0}",
        at(2026, 10, 1, 9, 30),
    );
    rtf.sub_kind = Some(ClipboardSubKind::Rtf);
    rtf.search_text = Some("Plain RTF v1.3.0".to_owned());
    rtf.summary = Some("Plain RTF v1.3.0".to_owned());
    items.push(rtf);

    let mut url = row(
        "text-url",
        ClipboardKind::Text,
        "https://example.com/a?b=1",
        at(2026, 9, 30, 8, 0),
    );
    url.sub_kind = Some(ClipboardSubKind::Url);
    items.push(url);

    let mut email = row(
        "text-email",
        ClipboardKind::Text,
        "user@example.com",
        at(2026, 9, 29, 8, 0),
    );
    email.sub_kind = Some(ClipboardSubKind::Email);
    items.push(email);

    let mut color = row(
        "text-color",
        ClipboardKind::Text,
        "rgb(255 136 0)",
        at(2026, 9, 28, 8, 0),
    );
    color.sub_kind = Some(ClipboardSubKind::Color);
    items.push(color);

    let mut path = row(
        "text-path",
        ClipboardKind::Text,
        "C:/Users/demo/report.docx",
        at(2026, 9, 27, 8, 0),
    );
    path.sub_kind = Some(ClipboardSubKind::Path);
    items.push(path);

    let mut secret = row(
        "text-secret",
        ClipboardKind::Text,
        "token sk-abcdefghijklmnopqrstuvwxyzABCDE1234567890\nsecond line",
        at(2026, 9, 26, 8, 0),
    );
    secret.is_sensitive = true;
    items.push(secret);

    let mut secret_color = row(
        "text-secret-color",
        ClipboardKind::Text,
        "#123456",
        at(2026, 9, 25, 8, 0),
    );
    secret_color.sub_kind = Some(ClipboardSubKind::Color);
    secret_color.is_sensitive = true;
    items.push(secret_color);

    let thumbed = images
        .store(&crate::clipboard::ImagePayload {
            bytes: super::tests::sample_png(640, 360),
            width: 640,
            height: 360,
        })
        .unwrap();
    images.ensure_thumbnail(&thumbed.file_name).unwrap();
    let mut image = row(
        "image-thumbnail",
        ClipboardKind::Image,
        &thumbed.file_name,
        at(2026, 9, 24, 8, 0),
    );
    image.width = Some(thumbed.width);
    image.height = Some(thumbed.height);
    image.size = Some(thumbed.size);
    items.push(image);

    let bare = images
        .store(&crate::clipboard::ImagePayload {
            bytes: super::tests::sample_png(20, 30),
            width: 20,
            height: 30,
        })
        .unwrap();
    let mut image = row(
        "image-no-thumbnail",
        ClipboardKind::Image,
        &bare.file_name,
        at(2025, 12, 31, 23, 0),
    );
    image.width = Some(bare.width);
    image.height = Some(bare.height);
    image.size = Some(bare.size);
    items.push(image);

    let mut files = row(
        "files-missing",
        ClipboardKind::Files,
        "C:/Missing/docs\nC:/Missing/readme.txt\nC:/Missing/photo.PNG",
        at(2025, 6, 1, 10, 0),
    );
    files.file_types = Some("d,f,f".to_owned());
    files.search_text = Some("docs\nreadme.txt\nphoto.PNG".to_owned());
    items.push(files);

    let photo = root.join("photos").join("cat.png");
    std::fs::create_dir_all(photo.parent().unwrap()).unwrap();
    std::fs::write(&photo, super::tests::sample_png(4, 4)).unwrap();
    let mut single = row(
        "files-image",
        ClipboardKind::Files,
        photo.to_str().unwrap(),
        at(2025, 6, 2, 10, 0),
    );
    single.file_types = Some("f".to_owned());
    single.search_text = Some("cat.png".to_owned());
    // 路径在临时目录下，每次不同；指纹固定下来，夹具才稳定。
    single.content_hash = content_hash(ClipboardKind::Files, "<root>/photos/cat.png");
    items.push(single);

    let mut synced = row(
        "text-synced",
        ClipboardKind::Text,
        "来自手机的文字",
        at(2026, 9, 23, 8, 0),
    );
    synced.origin_device_id = Some("device-phone".to_owned());
    items.push(synced);

    let mut grouped = row(
        "text-grouped",
        ClipboardKind::Text,
        "分组里的记录",
        at(2026, 9, 22, 8, 0),
    );
    grouped.group_id = Some("group-work".to_owned());
    grouped.use_count = 3;
    items.push(grouped);

    for item in &items {
        insert_item(&pool, item).await.unwrap();
    }

    Scene {
        _temp: temp,
        root,
        pool,
        images,
        app_icons,
        file_icons,
    }
}

impl Scene {
    fn context(&self, clipboard: &Clipboard) -> ListContext<'_, FixedOffset> {
        ListContext::new(
            &self.pool,
            &self.images,
            &self.app_icons,
            &self.file_icons,
            clipboard,
            now(),
        )
    }

    /// 与 1.4.0 `list_clipboard_items` 相同：查一页 → 逐条加工 → 组装分页。
    async fn list(&self, clipboard: &Clipboard) -> Value {
        let query = ClipboardItemQuery {
            limit: 50,
            ..ClipboardItemQuery::default()
        };
        let (rows, total) = crate::db::items::query_items_page(&self.pool, &query)
            .await
            .unwrap();
        let ctx = self.context(clipboard);
        let mut items = Vec::with_capacity(rows.len());
        for row in rows {
            items.push(present_list_item(&ctx, row).await.unwrap());
        }
        let has_more = query.offset + (items.len() as i64) < total;

        serde_json::to_value(ClipboardItemPage {
            list: items,
            total,
            has_more,
        })
        .unwrap()
    }

    /// 与 1.4.0 `get_clipboard_item` 相同：按 id 取列表视图并加工。
    async fn list_item(&self, id: &str, clipboard: &Clipboard) -> Value {
        let item = find_item_for_list_by_id(&self.pool, id)
            .await
            .unwrap()
            .unwrap();
        let view = present_list_item(&self.context(clipboard), item)
            .await
            .unwrap();
        serde_json::to_value(view).unwrap()
    }

    /// 与 1.4.0 `get_clipboard_preview_payload` 相同，按 id 汇总成一个对象。
    async fn previews(&self, redact: bool) -> Value {
        let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM clipboard_items ORDER BY id")
            .fetch_all(&self.pool)
            .await
            .unwrap();
        let mut out = BTreeMap::new();
        for id in ids {
            let item = find_item_by_id(&self.pool, &id).await.unwrap().unwrap();
            let payload =
                build_preview_payload(&self.pool, &self.images, &self.file_icons, item, redact)
                    .await
                    .unwrap();
            out.insert(id, serde_json::to_value(payload).unwrap());
        }
        serde_json::to_value(out).unwrap()
    }

    fn normalize(&self, value: Value) -> Value {
        let root = self.root.to_str().unwrap().to_owned();
        normalize(value, &root)
    }
}

fn normalize(value: Value, root: &str) -> Value {
    match value {
        Value::String(text) if text.starts_with(root) => {
            Value::String(format!("<root>{}", &text[root.len()..]).replace('\\', "/"))
        }
        Value::String(text) if text == "revealInFinder" => {
            Value::String("revealInExplorer".to_owned())
        }
        Value::Array(items) => {
            Value::Array(items.into_iter().map(|v| normalize(v, root)).collect())
        }
        Value::Object(fields) => Value::Object(
            fields
                .into_iter()
                .map(|(key, v)| (key, normalize(v, root)))
                .collect(),
        ),
        other => other,
    }
}

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/presenter")
        .join(name)
}

/// 与夹具比对；设置了 `KWIKPASTE_UPDATE_GOLDEN` 时改为写入夹具。
fn check(name: &str, actual: &Value) {
    let path = fixture_path(name);
    if std::env::var_os("KWIKPASTE_UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut json = serde_json::to_string_pretty(actual).unwrap();
        json.push('\n');
        std::fs::write(&path, json).unwrap();
        return;
    }

    let expected: Value = serde_json::from_str(
        &std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {path:?}: {err}")),
    )
    .unwrap();
    assert!(
        expected == *actual,
        "{name} differs from the golden fixture\nactual:\n{}",
        serde_json::to_string_pretty(actual).unwrap()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn list_and_preview_match_the_golden_fixtures() {
    let scene = scene().await;
    let redacted = Clipboard::default();
    let mut plain = Clipboard::default();
    plain.sensitive.redact_secrets = false;
    plain.display.quick_snippets = false;
    plain.display.file_max_count = 1;

    check(
        "list-default.json",
        &scene.normalize(scene.list(&redacted).await),
    );
    check(
        "list-unredacted.json",
        &scene.normalize(scene.list(&plain).await),
    );
    check(
        "list-item-secret.json",
        &scene.normalize(scene.list_item("text-secret", &redacted).await),
    );
    check(
        "preview-redacted.json",
        &scene.normalize(scene.previews(true).await),
    );
    check(
        "preview-unredacted.json",
        &scene.normalize(scene.previews(false).await),
    );
}
