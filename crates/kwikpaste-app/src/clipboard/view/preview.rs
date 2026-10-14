//! 预览窗（1.x `pages/Preview` 与 `window/preview.rs`）：卡片旁边的一个独立的不激活窗口，头部是标题、
//! 说明、文本方式切换和类型标签，内容是大图、长文本（纯文本或选词）、HTML / RTF 的纯文本、文件列表。
//!
//! 窗口启动后按需建一次、永不销毁；显示、隐藏、定位只走原生调用（Windows 与主面板一样是
//! `WS_EX_NOACTIVATE` 的 topmost 工具窗口），点它、滚它、选词都不抢前台。原生调用都放在任务里、
//! GPUI 的借用之外（见 `platform::panel` 的说明）。macOS 使用原生不激活 NSPanel。

use std::{ops::Range, rc::Rc, sync::Arc};

use gpui::{
    AnyElement, AnyWindowHandle, App, AppContext as _, Bounds, Context, Div, Entity, EventEmitter,
    FontWeight, ImageSource, InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent,
    MouseMoveEvent, ParentElement as _, Render, ScrollHandle, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled as _, UniformListScrollHandle, Window, WindowBounds,
    WindowKind, WindowOptions, div, img, point, prelude::FluentBuilder as _, px, size,
    uniform_list,
};
use kwikpaste_core::db::models::{ClipboardKind, ClipboardSubKind};
use kwikpaste_ui::{
    Button, Icon, IconName, KpStyled as _,
    theme::{self, TextSize, px_rems, radius, space},
};

use super::image_cache::{ImageKey, ImageState, KpImageCache, ResizeMode, path_of};
use crate::{
    clipboard::{
        model::preview::{
            FILE_ROW_HEIGHT, HEADER_HEIGHT, RectF, TEXT_PADDING_X, TEXT_ROW_HEIGHT,
            WORDS_BAR_HEIGHT, WordSelection, format_bytes, utf16_range,
        },
        source::{Preview, PreviewTextView},
    },
    i18n::{t, t_args, t_count},
    platform::window_drag::WindowDragArea as _,
};

/// 预览窗发给列表的事件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreviewEvent {
    /// 指针进出预览窗（悬停预览离开卡片后，指针在预览窗里就不关）。
    Pointer(bool),
    /// 切换文本方式（写进设置）。
    TextView(PreviewTextView),
    /// 选词后点了复制或粘贴。
    Words { paste: bool },
    /// 图片记录在「图片」和「图中文字」之间切换。
    ImageText(ImageTextView),
}

/// 有识别文字的图片记录正在看的那一面。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageTextView {
    Image,
    Text,
}

/// 预览窗的内容视图。
pub struct PreviewPanel {
    preview: Option<Preview>,
    /// 记录已经不在了：只显示空状态。
    missing: bool,
    text_view: PreviewTextView,
    rows: Rc<Vec<Range<usize>>>,
    selection: WordSelection,
    /// 图片在面板里的显示尺寸（逻辑像素），由列表按面板尺寸算好。
    image_box: Option<(f32, f32)>,
    /// 图片有识别文字时的「图片 / 文字」切换；`Text` 时 `preview` 是识别文字的文本预览。
    image_text: Option<ImageTextView>,
    images: Entity<KpImageCache>,
    /// 预览文件图标的独立有界缓存；关闭预览时不清空，避免再次打开时闪烁。
    icons: Entity<KpImageCache>,
    text_scroll: UniformListScrollHandle,
    scroll: ScrollHandle,
}

impl EventEmitter<PreviewEvent> for PreviewPanel {}

impl PreviewPanel {
    fn new(cx: &mut Context<Self>) -> Self {
        let images = cx.new(|_| KpImageCache::new());
        let icons = cx.new(|_| KpImageCache::with_capacity(128));
        cx.observe(&images, |_, _, cx| cx.notify()).detach();
        cx.observe(&icons, |_, _, cx| cx.notify()).detach();

        Self {
            preview: None,
            missing: false,
            text_view: PreviewTextView::default(),
            rows: Rc::default(),
            selection: WordSelection::default(),
            image_box: None,
            image_text: None,
            images,
            icons,
            text_scroll: UniformListScrollHandle::new(),
            scroll: ScrollHandle::new(),
        }
    }

    /// 换内容：选词清空，滚动回到顶部。`rows` 是纯文本视图按面板宽度折好的行。
    pub fn set(
        &mut self,
        preview: Option<Preview>,
        rows: Vec<Range<usize>>,
        text_view: PreviewTextView,
        image_box: Option<(f32, f32)>,
        image_text: Option<ImageTextView>,
        cx: &mut Context<Self>,
    ) {
        let same_item = self.image_text == image_text
            && matches!(
                (&self.preview, &preview),
                (Some(old), Some(new)) if old.payload.id == new.payload.id
            );
        if !same_item {
            self.selection.clear();
            self.scroll.set_offset(point(px(0.), px(0.)));
            self.text_scroll
                .scroll_to_item(0, gpui::ScrollStrategy::Top);
        }
        self.rows = Rc::new(rows);
        self.missing = preview.is_none();
        self.preview = preview;
        self.text_view = text_view;
        self.image_box = image_box;
        self.image_text = image_text;
        cx.notify();
    }

    pub fn item_id(&self) -> Option<&str> {
        self.preview
            .as_ref()
            .map(|preview| preview.payload.id.as_str())
    }

    /// 选词视图里选中的词（序号升序）；不在选词视图或没选时为空。
    pub fn selected_words(&self) -> Vec<usize> {
        if !self.words_view() {
            return Vec::new();
        }
        self.selection.selected().iter().copied().collect()
    }

    /// 释放图片（预览关上时）。
    pub fn release(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.selection.clear();
        self.images
            .update(cx, |images, cx| images.clear(Some(window), cx));
    }

    /// 窗口隐藏后丢掉正文：预览窗口只隐藏不关，留着的正文最多可到文本上限（默认 4 MB）。
    /// 不 `notify`，隐藏的窗口不用重画，下次 [`Self::set`] 会换上新内容。
    pub fn forget(&mut self) {
        self.preview = None;
        self.rows = Rc::default();
    }

    fn can_pick_words(&self) -> bool {
        self.preview.as_ref().is_some_and(|preview| {
            preview.payload.kind == ClipboardKind::Text && !preview.payload.words.is_empty()
        })
    }

    fn words_view(&self) -> bool {
        self.text_view == PreviewTextView::Words && self.can_pick_words()
    }

    fn header(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = theme::semantic(cx);
        let payload = self.preview.as_ref().map(|preview| &preview.payload);
        let (title, meta) = match payload {
            None => (t("preview:title.loading"), t("preview:meta.contentViewer")),
            Some(payload) => match payload.kind {
                ClipboardKind::Files => (
                    t_count(
                        "preview:title.files",
                        i64::try_from(payload.total_files).unwrap_or(i64::MAX),
                        &[],
                    ),
                    t_count(
                        "preview:meta.filesLoaded",
                        i64::try_from(payload.files.len()).unwrap_or(i64::MAX),
                        &[],
                    ),
                ),
                ClipboardKind::Image => {
                    let dimensions = match (payload.image_width, payload.image_height) {
                        (Some(width), Some(height)) => format!("{width} x {height}"),
                        _ => t("preview:meta.unknownSize").to_string(),
                    };
                    let size = payload
                        .size
                        .map(|size| format!(" · {}", format_bytes(size)))
                        .unwrap_or_default();
                    (
                        t("preview:title.image"),
                        SharedString::from(format!("{dimensions}{size}")),
                    )
                }
                ClipboardKind::Text if self.image_text == Some(ImageTextView::Text) => {
                    let units = payload
                        .text
                        .as_deref()
                        .map_or(0, |text| text.encode_utf16().count());
                    (
                        t("preview:title.imageText"),
                        t_count(
                            "preview:meta.characters",
                            i64::try_from(units).unwrap_or(i64::MAX),
                            &[],
                        ),
                    )
                }
                ClipboardKind::Text => {
                    let count = payload.size.unwrap_or_else(|| {
                        let units = payload
                            .text
                            .as_deref()
                            .map_or(0, |text| text.encode_utf16().count());
                        i64::try_from(units).unwrap_or(i64::MAX)
                    });
                    (
                        t("preview:title.text"),
                        t_count("preview:meta.characters", count, &[]),
                    )
                }
            },
        };

        div()
            .flex()
            .flex_none()
            .h(space((HEADER_HEIGHT as f32) / 4.))
            .items_center()
            .justify_between()
            .gap(space((12.) / 4.))
            .px(space((16.) / 4.))
            .border_b_1()
            .border_color(tokens.border.divider)
            .child(
                div()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .kp_text(TextSize::Sm)
                            .font_weight(FontWeight::MEDIUM)
                            .window_drag_area()
                            .child(title),
                    )
                    .child(
                        div()
                            .truncate()
                            .kp_text(TextSize::Xs)
                            .text_color(tokens.text.secondary)
                            .child(meta),
                    ),
            )
            .when_some(payload, |header, payload| {
                header.child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(space((8.) / 4.))
                        .when_some(self.image_text, |row, current| {
                            row.child(self.image_text_switch(current, cx))
                        })
                        .when(self.can_pick_words(), |row| row.child(self.view_switch(cx)))
                        // 有「图片 / 文字」切换时它已经说明了类型，不再放类型标签。
                        .when(self.image_text.is_none(), |row| {
                            row.child(
                                div()
                                    .flex()
                                    .h(space((24.) / 4.))
                                    .items_center()
                                    .rounded(radius::SM)
                                    .bg(tokens.fill.subtle)
                                    .px(space((8.) / 4.))
                                    .kp_text(TextSize::Xs)
                                    .text_color(tokens.text.secondary)
                                    .child(t(type_key(payload.kind, payload.sub_kind))),
                            )
                        }),
                )
            })
            .into_any_element()
    }

    /// 文本方式切换（1.x antd `Segmented size="small"`）。
    fn view_switch(&self, cx: &mut Context<Self>) -> AnyElement {
        let segment = |view: PreviewTextView, key: &str, cx: &mut Context<Self>| {
            segment(key, self.text_view == view, cx).on_click(cx.listener(
                move |panel, _, _, cx| {
                    if panel.text_view != view {
                        panel.text_view = view;
                        panel.selection.clear();
                        cx.emit(PreviewEvent::TextView(view));
                        cx.notify();
                    }
                },
            ))
        };

        segmented(
            [
                segment(PreviewTextView::Plain, "preview:view.plain", cx),
                segment(PreviewTextView::Words, "preview:view.words", cx),
            ],
            cx,
        )
    }

    /// 「图片 / 文字」切换：列表按选中的一面重新取内容、重排预览窗。
    fn image_text_switch(&self, current: ImageTextView, cx: &mut Context<Self>) -> AnyElement {
        let segment = |view: ImageTextView, key: &str, cx: &mut Context<Self>| {
            segment(key, current == view, cx).on_click(cx.listener(move |panel, _, _, cx| {
                if panel.image_text != Some(view) {
                    panel.selection.clear();
                    cx.emit(PreviewEvent::ImageText(view));
                }
            }))
        };

        segmented(
            [
                segment(ImageTextView::Image, "preview:view.image", cx),
                segment(ImageTextView::Text, "preview:view.text", cx),
            ],
            cx,
        )
    }

    fn empty(key: &str, cx: &App) -> AnyElement {
        let tokens = theme::semantic(cx);

        div()
            .flex()
            .flex_col()
            .size_full()
            .min_h(space((96.) / 4.))
            .items_center()
            .justify_center()
            .gap(space((8.) / 4.))
            .child(
                Icon::new(IconName::Inbox)
                    .size(space((32.) / 4.))
                    .color(tokens.text.faint),
            )
            .child(
                div()
                    .kp_text(TextSize::Sm)
                    .text_color(tokens.text.secondary)
                    .child(t(key)),
            )
            .into_any_element()
    }

    fn content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(preview) = self.preview.clone() else {
            return if self.missing {
                Self::empty("preview:empty.content", cx)
            } else {
                div().into_any_element()
            };
        };
        let payload = &preview.payload;

        match payload.kind {
            ClipboardKind::Image => self.image(&preview, window, cx),
            ClipboardKind::Files => self.files(&preview, window, cx),
            ClipboardKind::Text => {
                let text = payload.text.as_deref().unwrap_or_default();
                if text.is_empty() {
                    Self::empty("preview:empty.text", cx)
                } else if self.words_view() {
                    self.words(&preview, cx)
                } else {
                    self.plain_text(&preview, cx)
                }
            }
        }
    }

    fn image(
        &mut self,
        preview: &Preview,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let payload = &preview.payload;
        let (Some(path), true, Some((width, height))) = (
            payload.image_path.as_deref(),
            payload.image_exists,
            self.image_box,
        ) else {
            return Self::empty("preview:empty.imageMissing", cx);
        };
        let scale = window.scale_factor();
        let key = ImageKey {
            path: path_of(path),
            width: (width * scale).round().max(1.) as u32,
            height: (height * scale).round().max(1.) as u32,
            resize: ResizeMode::Exact,
        };
        let state = self
            .images
            .update(cx, |images, cx| images.request(key, window, cx));

        div()
            .flex()
            .size_full()
            .items_center()
            .justify_center()
            .p(space((16.) / 4.))
            .child(match state {
                // 同列表缩略图：圆角，叠一圈不占位置的细描边。
                ImageState::Ready(image) => div()
                    .relative()
                    .flex_none()
                    .w(px(width))
                    .h(px(height))
                    .child(
                        img(ImageSource::Render(image))
                            .size_full()
                            .rounded(radius::SM),
                    )
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full()
                            .rounded(radius::SM)
                            .border_1()
                            .border_color(theme::semantic(cx).border.divider),
                    )
                    .into_any_element(),
                ImageState::Loading => div().w(px(width)).h(px(height)).into_any_element(),
                ImageState::Failed => Self::empty("preview:empty.imageMissing", cx),
            })
            .into_any_element()
    }

    fn plain_text(&self, preview: &Preview, cx: &mut Context<Self>) -> AnyElement {
        let text: Arc<str> = Arc::from(preview.payload.text.as_deref().unwrap_or_default());
        let rows = self.rows.clone();

        uniform_list(
            "preview-text",
            rows.len(),
            cx.processor(move |_, range: Range<usize>, _, _| {
                range
                    .map(|index| {
                        let line = rows
                            .get(index)
                            .and_then(|row| text.get(row.clone()))
                            .filter(|line| !line.is_empty())
                            .unwrap_or(" ");
                        div()
                            .h(space((TEXT_ROW_HEIGHT as f32) / 4.))
                            .px(space((TEXT_PADDING_X as f32) / 4.))
                            .kp_text(TextSize::Sm)
                            .line_height(space((TEXT_ROW_HEIGHT as f32) / 4.))
                            .whitespace_nowrap()
                            .child(SharedString::from(line.to_owned()))
                    })
                    .collect()
            }),
        )
        .track_scroll(&self.text_scroll)
        .size_full()
        .py(space((16.) / 4.))
        .into_any_element()
    }

    fn words(&self, preview: &Preview, cx: &mut Context<Self>) -> AnyElement {
        let tokens = theme::semantic(cx);
        let payload = &preview.payload;
        let text = payload.text.as_deref().unwrap_or_default();
        let mut chips: Vec<AnyElement> = Vec::with_capacity(payload.words.len() + 8);
        let mut previous_end = 0;

        for (index, span) in payload.words.iter().enumerate() {
            let range = utf16_range(text, span.0, span.1);
            let between = text.get(previous_end.min(range.start)..range.start);
            if index > 0 && between.is_some_and(|between| between.contains('\n')) {
                chips.push(div().w_full().h(px(0.)).into_any_element());
            }
            previous_end = range.end;
            let selected = self.selection.is_selected(index);
            let word = SharedString::from(text.get(range).unwrap_or_default().to_owned());

            chips.push(
                div()
                    .id(("preview-word", index))
                    .min_w(space((24.) / 4.))
                    .max_w_full()
                    .px(space((6.) / 4.))
                    .py(space((2.) / 4.))
                    .rounded(radius::MD)
                    .kp_text(TextSize::Sm)
                    .cursor_pointer()
                    .map(|chip| {
                        if selected {
                            chip.bg(tokens.accent.solid)
                                .text_color(tokens.text.on_accent)
                        } else {
                            chip.bg(tokens.fill.subtle)
                                .hover(|style| style.bg(tokens.fill.default))
                        }
                    })
                    .child(div().text_center().child(word))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |panel, _: &MouseDownEvent, _, cx| {
                            panel.selection.press(index);
                            cx.notify();
                        }),
                    )
                    .on_mouse_move(cx.listener(move |panel, event: &MouseMoveEvent, _, cx| {
                        if event.pressed_button == Some(MouseButton::Left)
                            && panel.selection.dragging()
                        {
                            panel.selection.extend(index);
                            cx.notify();
                        }
                    }))
                    .into_any_element(),
            );
        }

        let count = self.selection.selected().len();
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .id("preview-words")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .content_start()
                            .gap(space(1.))
                            .p(space((16.) / 4.))
                            .children(chips),
                    )
                    .when(payload.words_truncated, |area| {
                        area.child(
                            div()
                                .px(space((16.) / 4.))
                                .pb(space((16.) / 4.))
                                .kp_text(TextSize::Xs)
                                .text_color(tokens.text.secondary)
                                .child(t("preview:words.truncated")),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(space((8.) / 4.))
                    .h(space((WORDS_BAR_HEIGHT as f32) / 4.))
                    .border_t_1()
                    .border_color(tokens.border.divider)
                    .pr(space((12.) / 4.))
                    .pl(space((16.) / 4.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .kp_text(TextSize::Xs)
                            .text_color(tokens.text.secondary)
                            .child(if count > 0 {
                                t_count(
                                    "preview:words.selected",
                                    i64::try_from(count).unwrap_or(i64::MAX),
                                    &[],
                                )
                            } else {
                                t("preview:words.hint")
                            }),
                    )
                    .when(count > 0, |bar| {
                        bar.child(
                            Button::new("preview-words-clear", t("preview:words.clear"))
                                .small()
                                .ghost()
                                .on_click(cx.listener(|panel, _, _, cx| {
                                    panel.selection.clear();
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("preview-words-copy", t("preview:words.copy"))
                                .small()
                                .on_click(cx.listener(|_, _, _, cx| {
                                    cx.emit(PreviewEvent::Words { paste: false });
                                })),
                        )
                        .child(
                            Button::new("preview-words-paste", t("preview:words.paste"))
                                .small()
                                .primary()
                                .on_click(cx.listener(|_, _, _, cx| {
                                    cx.emit(PreviewEvent::Words { paste: true });
                                })),
                        )
                    }),
            )
            .into_any_element()
    }

    fn icon(
        &mut self,
        path: &str,
        logical_size: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ImageState {
        let physical = (px_rems(logical_size).to_pixels(window.rem_size()).as_f32()
            * window.scale_factor())
        .ceil()
        .max(1.) as u32;
        let key = ImageKey {
            path: path_of(path),
            width: physical,
            height: physical,
            resize: ResizeMode::Contain,
        };
        self.icons
            .update(cx, |icons, cx| icons.request(key, window, cx))
    }

    fn files(
        &mut self,
        preview: &Preview,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = theme::semantic(cx);
        let payload = &preview.payload;
        if payload.files.is_empty() {
            return Self::empty("preview:empty.files", cx);
        }

        let icon_states: Vec<Option<ImageState>> = payload
            .files
            .iter()
            .map(|file| {
                file.icon_path
                    .as_deref()
                    .map(|path| self.icon(path, 24., window, cx))
            })
            .collect();
        let rows = payload
            .files
            .iter()
            .zip(icon_states)
            .map(|(file, icon_state)| {
                let kind = if file.is_dir {
                    t("preview:file.folder")
                } else {
                    t("preview:file.item")
                };
                let size_label = file
                    .size
                    .map(|size| SharedString::from(format_bytes(size)))
                    .unwrap_or(kind);
                let path = if file.exists {
                    SharedString::from(file.path.clone())
                } else {
                    t("preview:file.missingPath")
                };

                div().px(space((8.) / 4.)).child(
                    div()
                        .flex()
                        .min_h(space((FILE_ROW_HEIGHT as f32) / 4.))
                        .items_center()
                        .gap(space((8.) / 4.))
                        .rounded(radius::MD)
                        .px(space((8.) / 4.))
                        .py(space((6.) / 4.))
                        .when(!file.exists, |row| {
                            row.opacity(theme::components(cx).preview_file.missing_opacity)
                        })
                        .child(match (file.icon_path.is_some(), icon_state) {
                            (true, Some(ImageState::Ready(image))) => {
                                img(ImageSource::Render(image))
                                    .flex_none()
                                    .size(space((24.) / 4.))
                                    .into_any_element()
                            }
                            (true, Some(ImageState::Loading)) => {
                                div().size(space((24.) / 4.)).flex_none().into_any_element()
                            }
                            (true, Some(ImageState::Failed)) | (true, None) => {
                                div().size(space((24.) / 4.)).flex_none().into_any_element()
                            }
                            (false, _) => Icon::new(IconName::Folder)
                                .size(space((20.) / 4.))
                                .color(tokens.text.secondary)
                                .into_any_element(),
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .truncate()
                                        .kp_text(TextSize::Xs)
                                        .when(!file.exists, |name| name.line_through())
                                        .child(file.name.clone()),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .kp_text(TextSize::Xs)
                                        .text_color(tokens.text.secondary)
                                        .child(path),
                                ),
                        )
                        .child(
                            div()
                                .flex_none()
                                .kp_text(TextSize::Xs)
                                .text_color(tokens.text.muted)
                                .child(size_label),
                        ),
                )
            });

        div()
            .id("preview-files")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .py(space((8.) / 4.))
            .children(rows)
            .when(payload.total_files > payload.files.len(), |list| {
                list.child(
                    div()
                        .px(space((16.) / 4.))
                        .py(space((8.) / 4.))
                        .kp_text(TextSize::Xs)
                        .text_color(tokens.text.secondary)
                        .child(t_args(
                            "preview:file.shownCount",
                            &[
                                ("shown", &payload.files.len().to_string()),
                                ("total", &payload.total_files.to_string()),
                            ],
                        )),
                )
            })
            .into_any_element()
    }
}

/// 类型标签的文案 key（1.x `clipboard:types.{subKind ?? kind}`）。
/// 头部小号分段切换里的一段：选中段在亮色里是浮起的白块（带一层极淡的影），暗色里是亮一档的填充。
fn segment(key: &str, selected: bool, cx: &App) -> Stateful<Div> {
    let tokens = theme::semantic(cx);
    let thumb = match theme::appearance(cx) {
        theme::Appearance::Light => tokens.surface.panel,
        theme::Appearance::Dark => tokens.fill.default,
    };

    div()
        .id(SharedString::from(format!("preview-view-{key}")))
        .flex()
        .items_center()
        .h(space((20.) / 4.))
        .px(space((8.) / 4.))
        .rounded(radius::XS)
        .kp_text(TextSize::Xs)
        .cursor_pointer()
        .map(|segment| {
            if selected {
                segment
                    .bg(thumb)
                    .shadow(tokens.shadow.card.to_vec())
                    .text_color(tokens.text.primary)
            } else {
                segment
                    .text_color(tokens.text.secondary)
                    .hover(|style| style.text_color(tokens.text.primary))
            }
        })
        .child(t(key))
}

/// 分段切换的底框（1.x antd `Segmented size="small"`）。
fn segmented(segments: impl IntoIterator<Item = Stateful<Div>>, cx: &App) -> AnyElement {
    let tokens = theme::semantic(cx);

    div()
        .flex()
        .gap(space((2.) / 4.))
        .p(space((2.) / 4.))
        .rounded(radius::SM)
        .bg(tokens.fill.subtle)
        .children(segments)
        .into_any_element()
}

fn type_key(kind: ClipboardKind, sub_kind: Option<ClipboardSubKind>) -> &'static str {
    match (sub_kind, kind) {
        (Some(ClipboardSubKind::Rtf), _) => "clipboard:types.rtf",
        (Some(ClipboardSubKind::Html), _) => "clipboard:types.html",
        (Some(ClipboardSubKind::Url), _) => "clipboard:types.url",
        (Some(ClipboardSubKind::Email), _) => "clipboard:types.email",
        (Some(ClipboardSubKind::Color), _) => "clipboard:types.color",
        (Some(ClipboardSubKind::Path), _) => "clipboard:types.path",
        (None, ClipboardKind::Text) => "clipboard:types.text",
        (None, ClipboardKind::Image) => "clipboard:types.image",
        (None, ClipboardKind::Files) => "clipboard:types.files",
    }
}

impl Render for PreviewPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme::semantic(cx);
        let content = self.content(window, cx);

        div()
            .id("preview-panel")
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(crate::platform::material::shell_surface(
                cx,
                tokens.surface.raised,
            ))
            .border_1()
            .border_color(tokens.border.subtle)
            .text_color(tokens.text.primary)
            .on_hover(cx.listener(|_, hovered: &bool, _, cx| {
                cx.emit(PreviewEvent::Pointer(*hovered));
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|panel, _, _, _| panel.selection.release()),
            )
            .child(self.header(cx))
            .child(div().flex_1().min_h_0().relative().child(content))
    }
}

/// 预览窗：GPUI 窗口、内容视图和原生句柄。
pub struct PreviewWindow {
    pub handle: AnyWindowHandle,
    pub panel: Entity<PreviewPanel>,
    pub native: Rc<native::NativePreview>,
}

impl PreviewWindow {
    /// 以隐藏状态建窗。原生样式（不激活、topmost）要在借用之外补上，见 [`native::NativePreview::install`]。
    pub fn open(cx: &mut App) -> anyhow::Result<Self> {
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                point(px(0.), px(0.)),
                size(px(320.), px(240.)),
            ))),
            titlebar: None,
            focus: false,
            show: false,
            kind: WindowKind::PopUp,
            is_movable: true,
            is_resizable: false,
            is_minimizable: false,
            inactive_frame_interval: None,
            window_background: gpui::WindowBackgroundAppearance::Opaque,
            ..Default::default()
        };
        let mut native = None;
        let (handle, panel) = crate::platform::open_window(options, cx, |window, cx| {
            native = Some(native::NativePreview::attach(window));
            cx.new(PreviewPanel::new)
        })?;
        let native =
            native.ok_or_else(|| anyhow::anyhow!("the preview window was not built"))??;

        Ok(Self {
            handle,
            panel,
            native: Rc::new(native),
        })
    }
}

/// 原生预览窗（Windows）：与主面板同一套不激活的工具窗口。
#[cfg(target_os = "windows")]
pub mod native {
    use anyhow::{anyhow, bail};
    use gpui::{Bounds, Pixels, Window};
    use kwikpaste_os::geometry::{Point, Rect};
    use kwikpaste_os::win::{
        monitor::{self, MonitorInfo},
        panel::{self as win_panel, PanelOptions},
    };
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use super::ScreenPlace;
    use crate::clipboard::model::preview::RectF;

    pub struct NativePreview {
        panel: win_panel::Panel,
    }

    fn hwnd(window: &Window) -> anyhow::Result<isize> {
        let handle = HasWindowHandle::window_handle(window)
            .map_err(|err| anyhow!("window handle: {err:?}"))?;
        let RawWindowHandle::Win32(handle) = handle.as_raw() else {
            bail!("not a Win32 window");
        };
        Ok(handle.hwnd.get())
    }

    impl NativePreview {
        pub fn attach(window: &Window) -> anyhow::Result<Self> {
            // GPUI 在主线程建窗，预览窗永不销毁。
            Ok(Self {
                panel: unsafe { win_panel::Panel::from_raw(hwnd(window)?) },
            })
        }

        /// 补上不激活、topmost 的样式。要在 GPUI 的借用之外调用。
        pub fn install(&self) {
            if let Err(err) = self.panel.install(PanelOptions {
                min_logical_size: (1., 1.),
                text_scale: 1.,
            }) {
                log::error!("preview window setup failed: {err}");
            }
        }

        /// 把内容区放到 `client`（物理像素）并不激活地显示。要在 GPUI 的借用之外调用。
        pub fn show(&self, client: Rect, dpi: u32) {
            if let Err(err) = self.panel.place(client, dpi) {
                log::warn!("preview window could not be placed: {err}");
                return;
            }
            self.panel.show_without_activating();
        }

        /// 隐藏。要在 GPUI 的借用之外调用。
        pub fn hide(&self) {
            self.panel.hide();
        }

        pub fn is_visible(&self) -> bool {
            self.panel.is_visible()
        }

        /// 预览窗是不是前台窗口（自测核对“不抢前台”）。
        pub fn is_foreground(&self) -> bool {
            kwikpaste_os::win::foreground_window() == self.panel.raw()
        }
    }

    /// 窗口内容区左上角的屏幕坐标（物理像素）。
    fn client_origin(window: &Window) -> Option<Point> {
        let panel = unsafe { win_panel::Panel::from_raw(hwnd(window).ok()?) };
        let rect = panel.client_rect().ok()?;
        Some(Point {
            x: rect.left,
            y: rect.top,
        })
    }

    /// 包含这个点（物理像素）的显示器；都不包含时取第一块。
    fn monitor_at(point: Point) -> Option<MonitorInfo> {
        let monitors = monitor::all();
        monitors
            .iter()
            .copied()
            .find(|info| {
                let rect = info.monitor;
                point.x >= rect.left
                    && point.x < rect.right
                    && point.y >= rect.top
                    && point.y < rect.bottom
            })
            .or_else(|| monitors.first().copied())
    }

    /// 面板窗口里 `card`（逻辑像素）所在显示器上的几何。
    pub fn screen_place(window: &Window, card: Bounds<Pixels>) -> Option<ScreenPlace> {
        let origin = client_origin(window)?;
        let scale = f64::from(window.scale_factor());
        let to_screen = |value: Pixels| f64::from(value.as_f32()) * scale;
        let left = f64::from(origin.x) + to_screen(card.origin.x);
        let top = f64::from(origin.y) + to_screen(card.origin.y);
        let width = to_screen(card.size.width);
        let height = to_screen(card.size.height);
        let monitor = monitor_at(Point {
            x: (left + width / 2.) as i32,
            y: (top + height / 2.) as i32,
        })?;
        let monitor_scale = monitor.scale();

        Some(ScreenPlace {
            card: RectF {
                left: (left - f64::from(monitor.monitor.left)) / monitor_scale,
                top: (top - f64::from(monitor.monitor.top)) / monitor_scale,
                width: width / monitor_scale,
                height: height / monitor_scale,
            },
            monitor: RectF {
                left: 0.,
                top: 0.,
                width: f64::from(monitor.monitor.width()) / monitor_scale,
                height: f64::from(monitor.monitor.height()) / monitor_scale,
            },
            origin: (monitor.monitor.left, monitor.monitor.top),
            scale: monitor_scale,
            dpi: monitor.dpi,
        })
    }
}

/// 卡片所在显示器上的几何：卡片与显示器都是以显示器左上角为原点的逻辑像素。
pub struct ScreenPlace {
    pub card: RectF,
    pub monitor: RectF,
    /// 显示器左上角的屏幕坐标（物理像素）。
    pub origin: (i32, i32),
    /// 显示器的缩放比例。
    pub scale: f64,
    pub dpi: u32,
}

impl ScreenPlace {
    /// 显示器上的逻辑矩形换成屏幕上的物理矩形。
    pub fn to_screen(&self, rect: RectF) -> kwikpaste_os::geometry::Rect {
        let to_physical = |value: f64| (value * self.scale).round() as i32;
        let left = self.origin.0 + to_physical(rect.left);
        let top = self.origin.1 + to_physical(rect.top);
        kwikpaste_os::geometry::Rect {
            left,
            top,
            right: left + to_physical(rect.width),
            bottom: top + to_physical(rect.height),
        }
    }
}

#[cfg(target_os = "macos")]
pub mod native {
    use anyhow::{anyhow, bail};
    use gpui::{Bounds, Pixels, Window};
    use kwikpaste_os::geometry::{Rect, Size};
    use kwikpaste_os::mac::{monitor, panel as mac_panel};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use super::{RectF, ScreenPlace};

    pub struct NativePreview {
        panel: mac_panel::Panel,
    }

    /// 面板窗口里 `card`（逻辑像素，macOS 上就是 point）所在显示器上的几何。
    ///
    /// 全程用 point、以主屏左上角为原点（与 [`monitor::screens`] 相同）；只有交给
    /// [`ScreenPlace::to_screen`] 的显示器原点按缩放乘成像素，`place_rect` 再按同一缩放除回来。
    pub fn screen_place(window: &Window, card: Bounds<Pixels>) -> Option<ScreenPlace> {
        let handle = HasWindowHandle::window_handle(window).ok()?;
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return None;
        };
        let panel = unsafe { mac_panel::Panel::from_raw(handle.ns_view) };
        let frame = panel.frame()?;
        let screens = monitor::screens();
        let primary_height = screens.first()?.height;
        let left = frame.origin.x + f64::from(card.origin.x.as_f32());
        let top = primary_height - (frame.origin.y + frame.size.height)
            + f64::from(card.origin.y.as_f32());
        let width = f64::from(card.size.width.as_f32());
        let height = f64::from(card.size.height.as_f32());
        let monitor = screens
            .iter()
            .find(|screen| {
                left + width / 2. >= screen.x
                    && left + width / 2. < screen.x + screen.width
                    && top + height / 2. >= screen.y
                    && top + height / 2. < screen.y + screen.height
            })
            .or_else(|| screens.first())?;
        let monitor_scale = monitor.scale;
        Some(ScreenPlace {
            card: RectF {
                left: left - monitor.x,
                top: top - monitor.y,
                width,
                height,
            },
            monitor: RectF {
                left: 0.,
                top: 0.,
                width: monitor.width,
                height: monitor.height,
            },
            origin: (
                (monitor.x * monitor_scale).round() as i32,
                (monitor.y * monitor_scale).round() as i32,
            ),
            scale: monitor_scale,
            dpi: (monitor_scale * 96.).round() as u32,
        })
    }

    impl NativePreview {
        pub fn attach(window: &Window) -> anyhow::Result<Self> {
            let handle = HasWindowHandle::window_handle(window)
                .map_err(|err| anyhow!("preview window handle: {err:?}"))?;
            let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
                bail!("not an AppKit preview window");
            };
            Ok(Self {
                panel: unsafe { mac_panel::Panel::from_raw(handle.ns_view) },
            })
        }

        pub fn install(&self) {
            if let Err(err) = self.panel.install_preview(Size {
                width: 1,
                height: 1,
            }) {
                log::error!("preview window setup failed: {err}");
            }
        }

        pub fn show(&self, rect: Rect, dpi: u32) {
            if let Err(err) = self.panel.place_rect(rect, dpi) {
                log::warn!("preview window could not be placed: {err}");
                return;
            }
            self.panel.show_without_activating();
        }

        pub fn hide(&self) {
            self.panel.hide();
        }

        pub fn is_visible(&self) -> bool {
            self.panel.is_visible()
        }

        pub fn is_foreground(&self) -> bool {
            self.panel.is_key() && self.panel.application_is_active()
        }
    }
}

/// X11 预览窗不取焦点，避免按住空格预览时主面板失去键盘输入。
#[cfg(target_os = "linux")]
pub mod native {
    use super::ScreenPlace;
    use crate::clipboard::model::preview::RectF;
    use anyhow::{anyhow, bail};
    use gpui::{Bounds, Pixels, Window};
    use kwikpaste_os::{
        geometry::Rect,
        linux::{monitor, x11},
    };
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    pub struct NativePreview {
        xid: u32,
    }

    /// 提取预览和来源窗口的 X11 ID。
    fn xid(window: &Window) -> anyhow::Result<u32> {
        let handle = HasWindowHandle::window_handle(window)
            .map_err(|err| anyhow!("window handle: {err:?}"))?;
        match handle.as_raw() {
            RawWindowHandle::Xcb(handle) => Ok(handle.window.get()),
            RawWindowHandle::Xlib(handle) => Ok(u32::try_from(handle.window)?),
            _ => bail!("preview requires X11 or XWayland"),
        }
    }
    impl NativePreview {
        /// 预览窗口由 GPUI 持有，本结构只保存原生句柄。
        pub fn attach(window: &Window) -> anyhow::Result<Self> {
            Ok(Self { xid: xid(window)? })
        }
        /// 初始隐藏，首次预览时才映射。
        pub fn install(&self) {
            self.hide();
        }
        /// 使用物理像素定位并映射，不修改当前键盘焦点。
        pub fn show(&self, rect: Rect, _dpi: u32) {
            if let Err(err) = x11::place(self.xid, rect).and_then(|_| x11::show(self.xid, false)) {
                log::warn!("preview window could not be shown: {err}");
            }
        }
        /// 收起预览，错误仅写日志。
        pub fn hide(&self) {
            if let Err(err) = x11::hide(self.xid) {
                log::debug!("preview hide failed: {err}");
            }
        }
        /// 检查服务端真实可见状态。
        pub fn is_visible(&self) -> bool {
            x11::is_visible(self.xid)
        }
        /// 自测必须确认预览未抢夺输入焦点。
        pub fn is_foreground(&self) -> bool {
            x11::focused_window() == Some(self.xid)
        }
    }
    /// 卡片逻辑坐标先转物理屏幕坐标，再转换到所在显示器坐标系。
    pub fn screen_place(window: &Window, card: Bounds<Pixels>) -> Option<ScreenPlace> {
        let origin = x11::geometry(xid(window).ok()?).ok()?;
        let scale = f64::from(window.scale_factor());
        let left = f64::from(origin.left) + f64::from(card.origin.x.as_f32()) * scale;
        let top = f64::from(origin.top) + f64::from(card.origin.y.as_f32()) * scale;
        let width = f64::from(card.size.width.as_f32()) * scale;
        let height = f64::from(card.size.height.as_f32()) * scale;
        let monitors = monitor::all();
        let info = monitors
            .iter()
            .find(|info| {
                left >= f64::from(info.monitor.left)
                    && left < f64::from(info.monitor.right)
                    && top >= f64::from(info.monitor.top)
                    && top < f64::from(info.monitor.bottom)
            })
            .or(monitors.first())?;
        let monitor_scale = info.scale();
        Some(ScreenPlace {
            card: RectF {
                left: (left - f64::from(info.monitor.left)) / monitor_scale,
                top: (top - f64::from(info.monitor.top)) / monitor_scale,
                width: width / monitor_scale,
                height: height / monitor_scale,
            },
            monitor: RectF {
                left: 0.,
                top: 0.,
                width: f64::from(info.monitor.width()) / monitor_scale,
                height: f64::from(info.monitor.height()) / monitor_scale,
            },
            origin: (info.monitor.left, info.monitor.top),
            scale: monitor_scale,
            dpi: info.dpi,
        })
    }
}
