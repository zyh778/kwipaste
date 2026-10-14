//! 组件展示窗（UI 里程碑 U0）：`KWIKPASTE_SELFTEST=1 KwikPaste --selftest-gallery`。
//!
//! 分区展示冻结的色板、标准字号和 kwikpaste-ui 的全部包装组件，顶部可切换明暗、语言和文本大小。
//! 截图验收用的环境变量（只在这个自测里读）：
//! - `KP_GALLERY_THEME`：`system` / `light` / `dark`；
//! - `KP_GALLERY_LANG`：`zh-CN` / `en-US`；
//! - `KP_TEXT_SCALE`：模拟 Windows“文本大小”，如 `1.5`（不改系统设置）；
//! - `KP_GALLERY_DEMO`：逗号分隔，`toasts` 打开时弹出四种提示并停留 60 s，`confirm` 打开时弹出危险确认框，
//!   `bottom` 滚到页面底部（截下半部分）。
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::time::Duration;

use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Div, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, TitlebarOptions, Window,
    WindowBounds, WindowOptions, div, prelude::FluentBuilder as _, px, rems, size,
};
use kwikpaste_ui::{
    Button, Checkbox, ConfirmBody, ConfirmSpec, IconName, Input, KeyHint, KpStyled as _, Select,
    SelectOption, SelectState, Shortcut, Switch, Tag, TagColor, TextInput, TooltipBubble,
    theme::{self, TextSize, ThemePreference, radius, space},
    toast::{self, Toast, ToastCapsule, ToastKind},
};

use crate::{
    i18n::{self, Language, t, t_args, t_count},
    selftest,
};

/// 截图验收里提示停留的时长，足够从容截图。
const DEMO_TOAST_DURATION: Duration = Duration::from_secs(60);
/// 文本大小档位：1.x 测过的 100 / 125 / 150 / 225%。
const TEXT_SCALES: [f32; 4] = [1., 1.25, 1.5, 2.25];

#[cfg(any(target_os = "windows", target_os = "linux"))]
const MOD_KEY: &str = "Ctrl";
#[cfg(target_os = "macos")]
const MOD_KEY: &str = "⌘";

/// 请求了 `--selftest-gallery` 时打开展示窗并返回 true；否则什么也不做。
pub fn open_if_requested(cx: &mut App) -> bool {
    if !selftest::gallery_requested() {
        return false;
    }

    apply_env_overrides(cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(1200.), px(900.)), cx)),
        titlebar: Some(TitlebarOptions {
            title: Some(t("gallery:title")),
            ..Default::default()
        }),
        focus: false,
        ..Default::default()
    };
    let opened = crate::platform::open_window(options, cx, |window, cx| {
        cx.new(|cx| Gallery::new(window, cx))
    });
    if let Err(error) = opened {
        eprintln!("the gallery window failed to open: {error:#}");
    }

    true
}

/// 自测截图用的覆盖：`KP_GALLERY_THEME`（light/dark/system）、`KP_GALLERY_LANG`、`KP_TEXT_SCALE`。
/// 展示窗和列表自测（`--selftest-list-*`）共用。
pub fn apply_env_overrides(cx: &mut App) {
    let preference = match std::env::var("KP_GALLERY_THEME").as_deref() {
        Ok("light") => Some(ThemePreference::Light),
        Ok("dark") => Some(ThemePreference::Dark),
        Ok("system") => Some(ThemePreference::System),
        _ => None,
    };
    if let Some(preference) = preference {
        theme::set_preference(preference, cx);
    }

    if let Some(language) = std::env::var("KP_GALLERY_LANG")
        .ok()
        .as_deref()
        .and_then(Language::from_tag)
    {
        i18n::set_language(language, cx);
    }

    if let Some(scale) = std::env::var("KP_TEXT_SCALE")
        .ok()
        .and_then(|scale| scale.parse::<f32>().ok())
    {
        theme::set_text_scale(scale, cx);
    }
}

struct Gallery {
    focus_search: bool,
    clear_on_hide: bool,
    switch_on: bool,
    small_switch_on: bool,
    name_input: TextInput,
    search_input: TextInput,
    sort: SelectState,
    last_answer: Option<bool>,
    scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl Gallery {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name_input = TextInput::new(t("clipboard:groups.namePlaceholder"), window, cx);
        let search_input = TextInput::new(t("clipboard:header.searchPlaceholder"), window, cx);
        let sort = SelectState::new(sort_options(), Some("createdAtDesc"), window, cx);
        let subscriptions = vec![
            search_input.on_change(cx, |_, _, cx| cx.notify()),
            sort.on_change(cx, |_, _, cx| cx.notify()),
        ];

        // 提示和对话框都挂在窗口的 Root 上，要等 `open_window` 装好 Root 之后才能弹。
        let demo = std::env::var("KP_GALLERY_DEMO").unwrap_or_default();
        if !demo.is_empty() {
            cx.spawn_in(window, async move |this, cx| {
                let _ = this.update_in(cx, |this, window, cx| this.run_demo(&demo, window, cx));
            })
            .detach();
        }

        Self {
            focus_search: !crate::selftest::capture_mode(),
            clear_on_hide: false,
            switch_on: true,
            small_switch_on: false,
            name_input,
            search_input,
            sort,
            last_answer: None,
            scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        }
    }

    /// 截图验收：按 `KP_GALLERY_DEMO` 预先弹出提示、确认框或滚到底部。
    fn run_demo(&mut self, demo: &str, window: &mut Window, cx: &mut Context<Self>) {
        for item in demo.split(',').map(str::trim) {
            self.run_demo_item(item, window, cx);
        }
    }

    fn run_demo_item(&mut self, demo: &str, window: &mut Window, cx: &mut Context<Self>) {
        match demo {
            "toasts" => {
                for kind in ToastKind::ALL {
                    toast::show(
                        Toast::new(kind, toast_message(kind)).duration(DEMO_TOAST_DURATION),
                        window,
                        cx,
                    );
                }
            }
            "confirm" => self.open_confirm(true, window, cx),
            "bottom" => {
                self.scroll.scroll_to_bottom();
                cx.notify();
            }
            _ => {}
        }
    }

    fn set_language(&mut self, language: Language, window: &mut Window, cx: &mut Context<Self>) {
        i18n::set_language(language, cx);
        self.name_input
            .set_placeholder(t("clipboard:groups.namePlaceholder"), window, cx);
        self.search_input
            .set_placeholder(t("clipboard:header.searchPlaceholder"), window, cx);
        self.sort.set_options(sort_options(), window, cx);
        cx.notify();
    }

    fn open_confirm(&mut self, danger: bool, window: &mut Window, cx: &mut Context<Self>) {
        let answer = kwikpaste_ui::confirm(confirm_spec(danger), window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let confirmed = answer.await.unwrap_or(false);
            let _ = this.update(cx, |this, cx| {
                this.last_answer = Some(confirmed);
                cx.notify();
            });
        })
        .detach();
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme::semantic(cx);
        let preference = theme::preference(cx);
        let language = i18n::language();
        let scale = theme::text_scale(cx);
        let theme_options = [
            (
                ThemePreference::System,
                "preferences:schema.settings.appearance.theme.options.auto",
                IconName::Settings,
            ),
            (
                ThemePreference::Light,
                "preferences:schema.settings.appearance.theme.options.light",
                IconName::Sun,
            ),
            (
                ThemePreference::Dark,
                "preferences:schema.settings.appearance.theme.options.dark",
                IconName::Moon,
            ),
        ];

        row()
            .flex_wrap()
            .justify_between()
            .gap(space(6.))
            .px(space(6.))
            .py(space(4.))
            .bg(tokens.surface.panel)
            .border_b_1()
            .border_color(tokens.border.subtle)
            .child(
                column()
                    .gap(space(1.))
                    .child(
                        div()
                            .kp_text(TextSize::Lg)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(t("gallery:title")),
                    )
                    .child(
                        div()
                            .kp_text(TextSize::Xs)
                            .text_color(tokens.text.secondary)
                            .child(t("gallery:subtitle")),
                    ),
            )
            .child(
                row()
                    .flex_wrap()
                    .gap(space(6.))
                    .child(control_group(
                        t("preferences:schema.settings.appearance.theme.title"),
                        theme_options
                            .into_iter()
                            .map(|(option, key, icon)| {
                                choice(format!("theme-{option:?}"), t(key), preference == option)
                                    .with_icon(icon)
                                    .on_click(move |_, _, cx| theme::set_preference(option, cx))
                                    .into_any_element()
                            })
                            .collect(),
                    ))
                    .child(control_group(
                        t("preferences:schema.settings.appearance.language.title"),
                        Language::ALL
                            .into_iter()
                            .map(|option| {
                                choice(
                                    format!("language-{}", option.tag()),
                                    t(language_label_key(option)),
                                    language == option,
                                )
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    this.set_language(option, window, cx);
                                }))
                                .into_any_element()
                            })
                            .collect(),
                    ))
                    .child(control_group(
                        t("gallery:controls.textScale"),
                        TEXT_SCALES
                            .into_iter()
                            .map(|option| {
                                let percent = (option * 100.).round();
                                choice(
                                    format!("scale-{percent}"),
                                    format!("{percent}%"),
                                    (scale - option).abs() < 0.01,
                                )
                                .on_click(move |_, _, cx| theme::set_text_scale(option, cx))
                                .into_any_element()
                            })
                            .collect(),
                    )),
            )
    }

    fn render_palette(&self, cx: &App) -> AnyElement {
        let tokens = theme::semantic(cx);
        let swatches = [
            ("text", "colorText", tokens.text.primary),
            ("secondary", "colorTextSecondary", tokens.text.secondary),
            ("tertiary", "colorTextTertiary", tokens.text.muted),
            ("quaternary", "colorTextQuaternary", tokens.text.faint),
            ("disabled", "colorBorderDisabled", tokens.border_disabled),
            ("description", "colorTextDescription", tokens.text.muted),
            (
                "placeholder",
                "colorTextPlaceholder",
                tokens.text.placeholder,
            ),
            ("light_solid", "colorTextLightSolid", tokens.text.on_accent),
            ("primary", "colorPrimary", tokens.accent.solid),
            ("primary_hover", "colorPrimaryHover", tokens.accent.hover),
            ("primary_active", "colorPrimaryActive", tokens.accent.active),
            ("primary_bg", "colorPrimaryBg", tokens.accent.subtle),
            ("primary_border", "colorPrimaryBorder", tokens.accent.border),
            ("success", "colorSuccess", tokens.status.success.solid),
            ("warning", "colorWarning", tokens.status.warning.solid),
            (
                "warning_hover",
                "colorWarningHover",
                tokens.status.warning.hover,
            ),
            ("error", "colorError", tokens.status.danger.solid),
            ("error_hover", "colorErrorHover", tokens.status.danger.hover),
            ("info", "colorInfo", tokens.status.info.solid),
            ("bg_container", "colorBgContainer", tokens.surface.panel),
            ("bg_elevated", "colorBgElevated", tokens.surface.raised),
            ("bg_layout", "colorBgLayout", tokens.surface.window),
            ("bg_spotlight", "colorBgSpotlight", tokens.surface.spotlight),
            ("mask", "colorBgMask", tokens.surface.mask),
            ("text_hover", "colorBgTextHover", tokens.item.text_hover),
            ("white", "colorWhite", tokens.white),
            ("border", "colorBorder", tokens.border.default),
            (
                "border_secondary",
                "colorBorderSecondary",
                tokens.border.subtle,
            ),
            ("split", "colorSplit", tokens.border.divider),
            ("fill", "colorFill", tokens.fill.strong),
            ("fill_secondary", "colorFillSecondary", tokens.fill.default),
            ("fill_tertiary", "colorFillTertiary", tokens.fill.subtle),
            ("fill_quaternary", "colorFillQuaternary", tokens.fill.faint),
            ("blue_1", "blue-1", tokens.hues.blue_1),
            ("blue_6", "blue-6", tokens.hues.blue_6),
            ("cyan_6", "cyan-6", tokens.hues.cyan_6),
            ("orange_6", "orange-6", tokens.hues.orange_6),
            ("gold_3", "gold-3", tokens.hues.gold_3),
        ];

        section(
            t("gallery:sections.palette"),
            Some(t("gallery:palette.description")),
            cx,
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_x(space(3.))
                .gap_y(space(2.))
                .children(swatches.into_iter().map(|(name, source, color)| {
                    row()
                        .w(rems(10.5))
                        .gap(space(2.))
                        .child(
                            div()
                                .flex_none()
                                .size(space(7.))
                                .rounded(radius::MD)
                                .border_1()
                                .border_color(tokens.border.subtle)
                                .bg(color),
                        )
                        .child(
                            column()
                                .min_w_0()
                                .child(div().kp_text(TextSize::Xs).truncate().child(name))
                                .child(
                                    div()
                                        .kp_text(TextSize::Xs)
                                        .kp_mono()
                                        .text_color(tokens.text.muted)
                                        .truncate()
                                        .child(source),
                                ),
                        )
                })),
        )
        .into_any_element()
    }

    fn render_typography(&self, cx: &App) -> AnyElement {
        let tokens = theme::semantic(cx);
        let levels = [
            ("text", tokens.text.primary),
            ("secondary", tokens.text.secondary),
            ("tertiary", tokens.text.muted),
            ("quaternary", tokens.text.faint),
            ("disabled", tokens.border_disabled),
            ("primary", tokens.accent.solid),
            ("error", tokens.status.danger.solid),
        ];

        section(t("gallery:sections.typography"), None, cx)
            .children(TextSize::ALL.into_iter().map(|size| {
                let font = size.font_size().0 * 16.;
                let line = size.line_height().0 * 16.;
                column()
                    .gap(space(0.5))
                    .child(
                        div()
                            .kp_text(TextSize::Xs)
                            .kp_mono()
                            .text_color(tokens.text.muted)
                            .child(format!("{} · {font}/{line}px", size.class_name())),
                    )
                    .child(
                        div()
                            .kp_text(size)
                            .truncate()
                            .child(t("gallery:typography.sample")),
                    )
            }))
            .child(
                div()
                    .kp_text(TextSize::Sm)
                    .kp_mono()
                    .child(t("gallery:typography.mono")),
            )
            .child(
                div()
                    .kp_text(TextSize::Sm)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(t("gallery:typography.sample")),
            )
            .child(caption(t("gallery:typography.levels"), cx))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_x(space(4.))
                    .gap_y(space(1.))
                    .children(levels.into_iter().map(|(name, color)| {
                        div().kp_text(TextSize::Sm).text_color(color).child(name)
                    })),
            )
            .into_any_element()
    }

    fn render_buttons(&self, cx: &App) -> AnyElement {
        section(t("gallery:sections.buttons"), None, cx)
            .child(
                wrap_row()
                    .child(Button::new("btn-primary", t("common:actions.save")).primary())
                    .child(Button::new("btn-default", t("common:actions.cancel")))
                    .child(Button::new("btn-danger", t("common:actions.delete")).danger())
                    .child(Button::new("btn-ghost", t("common:actions.clear")).ghost())
                    .child(Button::new("btn-link", t("common:actions.open")).link()),
            )
            .child(caption(t("gallery:buttons.sizes"), cx))
            .child(
                wrap_row()
                    .child(Button::new("btn-md", t("common:actions.add")).primary())
                    .child(
                        Button::new("btn-sm", t("common:actions.add"))
                            .primary()
                            .small(),
                    )
                    .child(
                        Button::new("btn-xs", t("common:actions.add"))
                            .primary()
                            .xsmall(),
                    )
                    .child(Button::new("btn-loading", t("gallery:buttons.loading")).loading(true))
                    .child(
                        Button::new("btn-disabled", t("gallery:buttons.disabled")).disabled(true),
                    )
                    .child(
                        Button::new("btn-disabled-primary", t("gallery:buttons.disabled"))
                            .primary()
                            .disabled(true),
                    ),
            )
            .child(caption(t("gallery:buttons.icons"), cx))
            .child(
                wrap_row()
                    .child(Button::icon(
                        "icon-copy",
                        IconName::Copy,
                        t("commands:labels.copy"),
                    ))
                    .child(Button::icon(
                        "icon-delete",
                        IconName::Delete,
                        t("commands:labels.delete"),
                    ))
                    .child(Button::icon(
                        "icon-search",
                        IconName::Search,
                        t("clipboard:shortcuts.focusSearch"),
                    ))
                    .child(
                        Button::icon("icon-copy-xs", IconName::Copy, t("commands:labels.copy"))
                            .xsmall(),
                    )
                    .child(
                        Button::icon(
                            "icon-delete-xs",
                            IconName::Delete,
                            t("commands:labels.delete"),
                        )
                        .xsmall(),
                    )
                    .child(
                        Button::new("btn-with-icon", t("commands:labels.copy"))
                            .with_icon(IconName::Copy)
                            .small(),
                    ),
            )
            .child(caption(t("gallery:buttons.tooltip"), cx))
            .child(
                wrap_row()
                    .items_start()
                    .child(TooltipBubble::new(t("commands:labels.copy")))
                    .child(TooltipBubble::new(t("commands:deleteConfirm.content"))),
            )
            .into_any_element()
    }

    fn render_form(&self, cx: &mut Context<Self>) -> AnyElement {
        let this = cx.weak_entity();
        let set = move |update: fn(&mut Gallery, bool)| {
            let this = this.clone();
            move |checked: bool, _: &mut Window, cx: &mut App| {
                let _ = this.update(cx, |gallery, cx| {
                    update(gallery, checked);
                    cx.notify();
                });
            }
        };
        let search_value = self.search_input.value(cx);
        let sort_value = self.sort.selected_value(cx).unwrap_or_default();

        section(t("gallery:sections.form"), None, cx)
            .child(
                wrap_row()
                    .gap_x(space(6.))
                    .child(
                        Checkbox::new("check-focus")
                            .label(t("preferences:schema.settings.search.defaultFocus.title"))
                            .checked(self.focus_search)
                            .on_change(set(|gallery, checked| gallery.focus_search = checked)),
                    )
                    .child(
                        Checkbox::new("check-clear")
                            .label(t("preferences:schema.settings.search.clearOnHide.title"))
                            .checked(self.clear_on_hide)
                            .on_change(set(|gallery, checked| gallery.clear_on_hide = checked)),
                    )
                    .child(
                        Checkbox::new("check-disabled")
                            .label(t("gallery:form.disabled"))
                            .checked(true)
                            .disabled(true),
                    ),
            )
            .child(
                wrap_row()
                    .gap_x(space(6.))
                    .child(
                        Switch::new("switch-focus")
                            .label(t("preferences:schema.settings.search.defaultFocus.title"))
                            .checked(self.switch_on)
                            .on_change(set(|gallery, checked| gallery.switch_on = checked)),
                    )
                    .child(
                        Switch::new("switch-small")
                            .accessibility_label(t(
                                "preferences:schema.settings.search.clearOnHide.title",
                            ))
                            .small()
                            .checked(self.small_switch_on)
                            .on_change(set(|gallery, checked| gallery.small_switch_on = checked)),
                    )
                    .child(
                        Switch::new("switch-disabled")
                            .label(t("gallery:form.disabled"))
                            .checked(true)
                            .disabled(true),
                    ),
            )
            .child(labeled(
                t("gallery:form.input"),
                Input::new(&self.name_input).width(rems(15.)),
                cx,
            ))
            .child(labeled(
                t("gallery:form.search"),
                row()
                    .gap(space(3.))
                    .child(Input::search(&self.search_input).small().width(rems(10.)))
                    .child(
                        div()
                            .kp_text(TextSize::Xs)
                            .text_color(theme::semantic(cx).text.muted)
                            .child(format!("\u{201c}{search_value}\u{201d}")),
                    ),
                cx,
            ))
            .child(labeled(
                t("preferences:schema.settings.search.sort.title"),
                row()
                    .gap(space(3.))
                    .child(Select::new(&self.sort).width(rems(10.)))
                    .child(
                        div()
                            .kp_text(TextSize::Xs)
                            .kp_mono()
                            .text_color(theme::semantic(cx).text.muted)
                            .child(sort_value),
                    ),
                cx,
            ))
            .into_any_element()
    }

    fn render_tags(&self, cx: &App) -> AnyElement {
        let shortcuts = [
            ("clipboard:shortcuts.focusSearch", vec![MOD_KEY, "F"]),
            ("clipboard:shortcuts.copySelected", vec![MOD_KEY, "C"]),
            ("clipboard:shortcuts.navigate", vec!["↑", "/", "↓"]),
        ];

        section(t("gallery:sections.tags"), None, cx)
            .child(caption(t("gallery:tags.tags"), cx))
            .child(
                wrap_row().gap(space(2.)).children(
                    TagColor::ALL
                        .into_iter()
                        .map(|color| Tag::new(t(tag_label_key(color))).color(color)),
                ),
            )
            .child(caption(t("gallery:tags.kbd"), cx))
            .children(shortcuts.into_iter().map(|(label, keys)| {
                row()
                    .justify_between()
                    .gap(space(3.))
                    .child(div().kp_text(TextSize::Sm).child(t(label)))
                    .child(Shortcut::new(keys))
            }))
            .child(caption(t("gallery:tags.keyHint"), cx))
            .child(
                wrap_row()
                    .gap(space(2.))
                    .children(["F", "1", "0", "K"].map(KeyHint::new)),
            )
            .into_any_element()
    }

    fn render_feedback(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = theme::semantic(cx);
        let answer = match self.last_answer {
            None => t("gallery:feedback.none"),
            Some(true) => t("gallery:feedback.confirmed"),
            Some(false) => t("gallery:feedback.cancelled"),
        };

        section(t("gallery:sections.feedback"), None, cx)
            .child(caption(t("gallery:feedback.toast"), cx))
            .child(wrap_row().children(ToastKind::ALL.into_iter().map(|kind| {
                Button::new(
                    SharedString::from(format!("toast-{kind:?}")),
                    format!("{kind:?}"),
                )
                .small()
                .on_click(move |_, window, cx| {
                    toast::show(
                        Toast::new(kind, toast_message(kind)).key(format!("{kind:?}")),
                        window,
                        cx,
                    );
                })
            })))
            .child(
                column().items_start().gap(space(2.)).children(
                    ToastKind::ALL.map(|kind| ToastCapsule::new(kind, toast_message(kind))),
                ),
            )
            .child(caption(t("gallery:feedback.confirm"), cx))
            .child(
                wrap_row()
                    .child(
                        Button::new("confirm-normal", t("gallery:feedback.confirm"))
                            .small()
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.open_confirm(false, window, cx);
                            })),
                    )
                    .child(
                        Button::new("confirm-danger", t("commands:deleteConfirm.title"))
                            .small()
                            .danger()
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.open_confirm(true, window, cx);
                            })),
                    )
                    .child(
                        div()
                            .kp_text(TextSize::Xs)
                            .text_color(tokens.text.secondary)
                            .child(t_args(
                                "gallery:feedback.result",
                                &[("result", answer.as_ref())],
                            )),
                    ),
            )
            .child(
                div()
                    .w(rems(26.))
                    .max_w_full()
                    .px(rems(1.5))
                    .py(rems(1.25))
                    .rounded(radius::LG)
                    .bg(tokens.surface.raised)
                    .border_1()
                    .border_color(tokens.border.subtle)
                    .shadow(tokens.shadow.overlay.to_vec())
                    .child(ConfirmBody::preview(confirm_spec(true), cx)),
            )
            .into_any_element()
    }
}

impl Render for Gallery {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme::semantic(cx);

        column()
            .size_full()
            .bg(crate::platform::material::shell_surface(
                cx,
                tokens.surface.window,
            ))
            .child(self.render_header(cx))
            .child(
                div()
                    .id("gallery-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(
                        row()
                            .items_start()
                            .gap(space(4.))
                            .p(space(6.))
                            .child(column().flex_1().min_w_0().child(self.render_palette(cx)))
                            .child(
                                column()
                                    .flex_1()
                                    .min_w_0()
                                    .gap(space(4.))
                                    .child(self.render_typography(cx))
                                    .child(self.render_buttons(cx))
                                    .child(self.render_tags(cx)),
                            )
                            .child(
                                column()
                                    .flex_1()
                                    .min_w_0()
                                    .gap(space(4.))
                                    .child(self.render_form(cx))
                                    .child(self.render_feedback(cx)),
                            ),
                    ),
            )
    }
}

fn row() -> Div {
    div().flex().flex_row().items_center()
}

fn column() -> Div {
    div().flex().flex_col()
}

fn wrap_row() -> Div {
    row().flex_wrap().gap(space(2.))
}

/// 一个分区卡片：标题、可选说明，内容由调用方追加。
fn section(title: SharedString, description: Option<SharedString>, cx: &App) -> Div {
    let tokens = theme::semantic(cx);

    column()
        .gap(space(3.))
        .p(space(4.))
        .rounded(radius::LG)
        .bg(tokens.surface.panel)
        .border_1()
        .border_color(tokens.border.subtle)
        .child(
            div()
                .kp_text(TextSize::Base)
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
        .when_some(description, |section, description| {
            section.child(
                div()
                    .kp_text(TextSize::Xs)
                    .text_color(tokens.text.muted)
                    .child(description),
            )
        })
}

fn caption(text: SharedString, cx: &App) -> Div {
    div()
        .kp_text(TextSize::Xs)
        .text_color(theme::semantic(cx).text.secondary)
        .child(text)
}

fn labeled(label: SharedString, control: impl IntoElement, cx: &App) -> Div {
    column()
        .gap(space(1.5))
        .child(caption(label, cx))
        .child(control)
}

/// 顶部的一组互斥选项按钮，选中的那个用主色。
fn control_group(label: SharedString, choices: Vec<AnyElement>) -> Div {
    column()
        .gap(space(1.))
        .child(div().kp_text(TextSize::Xs).child(label))
        .child(row().gap(space(1.)).children(choices))
}

fn choice(id: String, label: impl Into<SharedString>, selected: bool) -> Button {
    let button = Button::new(SharedString::from(id), label).small();
    if selected {
        return button.primary();
    }

    button
}

fn language_label_key(language: Language) -> &'static str {
    match language {
        Language::ZhCn => "preferences:schema.settings.appearance.language.options.zh-CN",
        Language::EnUs => "preferences:schema.settings.appearance.language.options.en-US",
    }
}

fn sort_options() -> Vec<SelectOption> {
    [
        (
            "createdAtDesc",
            "preferences:schema.settings.search.sort.options.createdAtDesc",
        ),
        (
            "updatedAtDesc",
            "preferences:schema.settings.search.sort.options.updatedAtDesc",
        ),
        (
            "useCountDesc",
            "preferences:schema.settings.search.sort.options.useCountDesc",
        ),
    ]
    .into_iter()
    .map(|(value, key)| SelectOption::new(value, t(key)))
    .collect()
}

fn tag_label_key(color: TagColor) -> &'static str {
    match color {
        TagColor::Default => "gallery:tags.colors.default",
        TagColor::Primary => "gallery:tags.colors.primary",
        TagColor::Success => "gallery:tags.colors.success",
        TagColor::Warning => "gallery:tags.colors.warning",
        TagColor::Error => "gallery:tags.colors.error",
    }
}

fn toast_message(kind: ToastKind) -> SharedString {
    match kind {
        ToastKind::Info => t("gallery:feedback.info"),
        ToastKind::Success => t_count("commands:messages.itemsDeleted", 3, &[]),
        ToastKind::Warning => t("gallery:feedback.warning"),
        ToastKind::Error => {
            let label = t("commands:labels.copy");
            let message = t("gallery:feedback.errorMessage");
            t_args(
                "commands:error",
                &[("label", label.as_ref()), ("message", message.as_ref())],
            )
        }
    }
}

fn confirm_spec(danger: bool) -> ConfirmSpec {
    if danger {
        return ConfirmSpec::new(t("commands:deleteConfirm.title"))
            .content(t("commands:deleteConfirm.content"))
            .ok_text(t("common:actions.delete"))
            .danger();
    }

    ConfirmSpec::new(t("gallery:feedback.confirm")).content(t("gallery:feedback.info"))
}
