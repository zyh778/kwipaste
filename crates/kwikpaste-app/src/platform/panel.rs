//! 剪贴板面板：启动时以隐藏状态预创建、永不销毁；显示、隐藏、定位只走原生调用，从不激活。
//!
//! 热键、托盘、第二实例、钩子和 UI 自己的请求都送进同一个 channel，由一个 `cx.spawn` 循环按顺序
//! 执行。原生调用都在 `cx.update` 之外：GPUI 的窗口过程因此能同步拿到 `App`，`ShowWindow` 发出的
//! `WM_SHOWWINDOW` 会当场画出首帧，改位置、改尺寸、激活的回调也不会因为借用冲突被丢掉。

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use anyhow::Context as _;
use async_channel::{Receiver, Sender};
use gpui::{
    AnyView, AnyWindowHandle, App, AppContext as _, AsyncApp, Bounds, Capslock, Context, Entity,
    EventEmitter, FocusHandle, Global, InteractiveElement as _, IntoElement, Modifiers,
    ModifiersChangedEvent, ParentElement as _, PlatformInput, Render, Styled as _, Window,
    WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, div, point, px, size,
};
use kwikpaste_core::settings::ListLayout;
use kwikpaste_os::clock;

use super::editing::EditTrigger;
use super::material::WindowMaterial;
use super::native::NativePanel;
use super::{probe, window_state};

/// 单栏面板的默认内容区尺寸（逻辑像素），与 1.x 相同；Windows 上再乘系统「文本大小」。
pub const PANEL_SIZE: (f64, f64) = (360.0, 600.0);
/// 单栏面板能缩到的最小内容区尺寸（逻辑像素）。
const PANEL_MIN_SIZE: (f64, f64) = (280.0, 400.0);

/// 当前布局的默认内容区尺寸（逻辑像素，未乘 Windows 文本缩放）：双栏是两个单栏宽。
pub fn panel_default_size(layout: ListLayout) -> (f64, f64) {
    columns(PANEL_SIZE, layout)
}

/// 当前布局的最小内容区尺寸（逻辑像素，未乘 Windows 文本缩放）。
pub fn panel_min_size(layout: ListLayout) -> (f64, f64) {
    columns(PANEL_MIN_SIZE, layout)
}

fn columns(single: (f64, f64), layout: ListLayout) -> (f64, f64) {
    match layout {
        ListLayout::Single => single,
        ListLayout::Split => (single.0 * 2.0, single.1),
    }
}

/// 显示 / 隐藏请求的来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerSource {
    Hotkey,
    Tray,
    SecondInstance,
    OutsideClick,
    #[cfg_attr(
        not(target_os = "windows"),
        expect(dead_code, reason = "Win+V 只在 Windows 上")
    )]
    WinV,
    #[cfg_attr(
        target_os = "linux",
        expect(dead_code, reason = "Linux 尚未接全局鼠标钩子")
    )]
    MouseButton,
    Ui,
    /// 粘贴前让出前台。
    Paste,
    /// 复制后按设置隐藏。
    Copy,
    Selftest,
}

impl TriggerSource {
    pub fn name(self) -> &'static str {
        match self {
            Self::Hotkey => "hotkey",
            Self::Tray => "tray",
            Self::SecondInstance => "second-instance",
            Self::OutsideClick => "outside-click",
            Self::WinV => "win-v",
            Self::MouseButton => "mouse-button",
            Self::Ui => "ui",
            Self::Paste => "paste",
            Self::Copy => "copy",
            Self::Selftest => "selftest",
        }
    }
}

/// 一次请求的来源和发出时刻（[`clock::now_ticks`]），用来量“触发到首帧”。
#[derive(Debug, Clone, Copy)]
pub struct Trigger {
    pub source: TriggerSource,
    pub ticks: i64,
}

impl Trigger {
    pub fn now(source: TriggerSource) -> Self {
        Self {
            source,
            ticks: clock::now_ticks(),
        }
    }
}

/// 送给面板循环的命令。
#[derive(Debug, Clone)]
pub enum PanelCommand {
    Toggle(Trigger),
    Show(Trigger),
    Hide(Trigger),
    /// 进入编辑态；结果以 [`PanelEvent::EditingStarted`] 或 [`PanelEvent::EditingRefused`] 送回。
    BeginEditing(EditTrigger),
    /// 退出编辑态，前台还给进入前的窗口；完成后发 [`PanelEvent::EditingEnded`]。
    EndEditing,
    /// 系统「文本大小」变了，面板的最小、默认尺寸跟着补偿。
    #[cfg_attr(
        not(target_os = "windows"),
        expect(dead_code, reason = "此平台不订阅独立的文本缩放设置")
    )]
    SetTextScale(f64),
    /// 切换单栏或双栏，立即更新原生最小尺寸与记住的宽度。
    SetLayout(ListLayout),
    /// 应用当前窗口材质；macOS 需要由持有原生面板的命令循环更新 NSVisualEffectView。
    SetMaterial(WindowMaterial),
    /// 粘贴前让出前台：编辑态先把前台还给进入前的窗口，`keep_visible` 为假时再隐藏面板。
    /// 处理完后经 `done` 回报面板此前是否可见（见 [`super::paste`]）。
    YieldForPaste {
        keep_visible: bool,
        done: Sender<bool>,
    },
    /// 点击面板外部时是否隐藏（默认是）。UI 在固定面板、打开系统文件对话框期间关掉它。
    SetHideOnOutsideClick(bool),
    /// 非激活窗口上的鼠标按下重新捕获或释放导航键。
    SetInputCapture(bool),
    /// 原生层截获了面板拖动区/缩放边的按下，GPUI 没有机会让弹出菜单自行收起。
    #[cfg_attr(
        target_os = "linux",
        expect(dead_code, reason = "Linux 通过 GPUI 管理弹出菜单")
    )]
    DismissPopup,
}

/// 面板状态变化，从 [`Panel::events`] 发出。
///
/// `Shown` 在原生显示之前、与重置焦点同一次 update 里发出，订阅者在这里重置的视图状态会进首帧；
/// `Hidden` 在原生隐藏之后发出，订阅者据此停止刷新、释放缓存。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelEvent {
    Shown,
    Hidden,
    /// 面板已是前台窗口，可以聚焦输入框。
    EditingStarted,
    /// 没能进入编辑态（面板不可见、没拿到前台），焦点留在原处。
    EditingRefused,
    /// 已退出编辑态（包括随面板隐藏退出），焦点应还给列表。
    EditingEnded,
    /// 原生鼠标按下发生在菜单之外（包括不激活的拖动/缩放区域）。
    PopupDismissed,
}

/// 发出 [`PanelEvent`] 的实体。面板窗口打开之前就已建好，UI 在构造自己的视图时即可订阅。
pub struct PanelEvents;

impl EventEmitter<PanelEvent> for PanelEvents {}

/// 面板窗口的根视图：包一层 UI 的视图，提供兜底焦点和首帧计时。
pub struct PanelRoot {
    content: AnyView,
    focus: FocusHandle,
}

impl Render for PanelRoot {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        frame_rendered();

        let root = div()
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .child(self.content.clone())
            .children(resize_edges(window));
        // X11：面板失焦后点回面板不会自动拿回输入焦点（override-redirect 窗口没有
        // 窗口管理器代劳），之后点外部也就收不到 FocusOut、面板不再自动隐藏。
        // 每次面板内按下时把焦点补回来，失焦隐藏的链条就不断了。
        #[cfg(target_os = "linux")]
        let root = root.on_mouse_down(gpui::MouseButton::Left, |_, window, _| {
            let Some(xid) = super::native::window_xid(window) else {
                return;
            };
            if kwikpaste_os::linux::x11::focused_window() != Some(xid) {
                let _ = kwikpaste_os::linux::x11::focus(xid);
            }
        });
        root
    }
}

/// 面板四边与四角的缩放热区（仅 Linux）：override-redirect 窗口没有窗口管理器给的
/// 缩放边，自己在内容外圈叠一层窄条，按下时开轮询手势直接改窗口几何。
#[cfg(target_os = "linux")]
fn resize_edges(window: &mut Window) -> Vec<gpui::AnyElement> {
    use gpui::{CursorStyle, MouseButton, Pixels};
    use kwikpaste_os::linux::x11;

    let Some(xid) = super::native::window_xid(window) else {
        return Vec::new();
    };
    // 最小尺寸按当前缩放换算成物理像素，与 window_options 的 window_min_size 一致。
    let scale = f64::from(window.scale_factor());
    let min = ((PANEL_SIZE.0 * scale) as i32, (PANEL_SIZE.1 * scale) as i32);

    const WIDTH: Pixels = px(5.);
    const CORNER: Pixels = px(10.);
    fn strip(
        xid: u32,
        edge: x11::ResizeEdge,
        cursor: CursorStyle,
        min: (i32, i32),
        place: fn(gpui::Div) -> gpui::Div,
    ) -> gpui::AnyElement {
        place(div().absolute())
            .cursor(cursor)
            .on_mouse_down(MouseButton::Left, move |_, _, _| {
                x11::begin_resize(xid, edge, min);
            })
            .into_any_element()
    }

    use gpui::Div;
    vec![
        strip(xid, x11::ResizeEdge::Left, CursorStyle::ResizeLeftRight, min, |d: Div| {
            d.left_0().top_0().bottom_0().w(WIDTH)
        }),
        strip(xid, x11::ResizeEdge::Right, CursorStyle::ResizeLeftRight, min, |d: Div| {
            d.right_0().top_0().bottom_0().w(WIDTH)
        }),
        strip(xid, x11::ResizeEdge::Top, CursorStyle::ResizeUpDown, min, |d: Div| {
            d.top_0().left_0().right_0().h(WIDTH)
        }),
        strip(xid, x11::ResizeEdge::Bottom, CursorStyle::ResizeUpDown, min, |d: Div| {
            d.bottom_0().left_0().right_0().h(WIDTH)
        }),
        strip(xid, x11::ResizeEdge::TopLeft, CursorStyle::ResizeUpLeftDownRight, min, |d: Div| {
            d.top_0().left_0().w(CORNER).h(CORNER)
        }),
        strip(xid, x11::ResizeEdge::TopRight, CursorStyle::ResizeUpRightDownLeft, min, |d: Div| {
            d.top_0().right_0().w(CORNER).h(CORNER)
        }),
        strip(xid, x11::ResizeEdge::BottomLeft, CursorStyle::ResizeUpRightDownLeft, min, |d: Div| {
            d.bottom_0().left_0().w(CORNER).h(CORNER)
        }),
        strip(xid, x11::ResizeEdge::BottomRight, CursorStyle::ResizeUpLeftDownRight, min, |d: Div| {
            d.bottom_0().right_0().w(CORNER).h(CORNER)
        }),
    ]
}

/// 面板的全局句柄。面板窗口打开之前就挂上（只是还没有窗口），UI 的视图构造时就能订阅事件、发命令。
pub struct Panel {
    events: Entity<PanelEvents>,
    window: Option<AnyWindowHandle>,
    commands: Sender<PanelCommand>,
}

impl Global for Panel {}

impl Panel {
    pub fn commands(&self) -> Sender<PanelCommand> {
        self.commands.clone()
    }

    /// 发一条命令，在下一轮主循环里执行。
    pub fn request(&self, command: PanelCommand) {
        if let Err(err) = self.commands.try_send(command) {
            log::warn!(
                "panel command loop has stopped; dropped {:?}",
                err.into_inner()
            );
        }
    }

    /// 订阅 [`PanelEvent`] 用。
    pub fn events(&self) -> &Entity<PanelEvents> {
        &self.events
    }

    /// 面板窗口，钩子按键派发用；窗口打开之前为 `None`。
    #[cfg_attr(
        not(target_os = "windows"),
        expect(dead_code, reason = "此平台不用键盘钩子")
    )]
    pub fn window(&self) -> Option<AnyWindowHandle> {
        self.window
    }
}

thread_local! {
    static FIRST_FRAME: Cell<Option<Sender<i64>>> = const { Cell::new(None) };
}

static RENDERED_FRAMES: AtomicU64 = AtomicU64::new(0);
/// 面板当前是否显示着（崩溃记录的阶段用）。
static SHOWN: AtomicBool = AtomicBool::new(false);

/// 面板当前是否显示着。
pub fn is_shown() -> bool {
    SHOWN.load(Ordering::SeqCst)
}

/// 面板根视图渲染过的帧数。
pub fn rendered_frames() -> u64 {
    RENDERED_FRAMES.load(Ordering::Relaxed)
}

fn frame_rendered() {
    RENDERED_FRAMES.fetch_add(1, Ordering::Relaxed);
    if let Some(sender) = FIRST_FRAME.take() {
        let _ = sender.try_send(clock::now_ticks());
    }
}

/// 下一次渲染面板根视图时，把渲染时刻送进返回的 channel。
fn arm_first_frame() -> Receiver<i64> {
    let (sender, receiver) = async_channel::bounded(1);
    FIRST_FRAME.set(Some(sender));
    receiver
}

fn window_options(layout: ListLayout) -> WindowOptions {
    let default = panel_default_size(layout);
    let min = panel_min_size(layout);
    let panel_size = size(px(default.0 as f32), px(default.1 as f32));

    WindowOptions {
        // 隐藏建窗时 GPUI 不应用这里的位置；每次显示都由原生代码重设几何。
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(0.), px(0.)),
            panel_size,
        ))),
        titlebar: None,
        focus: false,
        show: false,
        kind: WindowKind::PopUp,
        is_movable: true,
        // macOS 面板顶部的隐藏标题栏不归系统管：双击不再按系统设置缩放到铺满屏幕，
        // 拖动只走头部的 `window_drag_area`。
        app_owns_titlebar_drag: true,
        is_resizable: true,
        is_minimizable: false,
        // 面板从不激活；默认值会把非活动窗口的动画压到 30 fps。面板因此不能放常驻动画。
        inactive_frame_interval: None,
        window_min_size: Some(size(px(min.0 as f32), px(min.1 as f32))),
        window_background: WindowBackgroundAppearance::Opaque,
        ..Default::default()
    }
}

/// 以隐藏状态创建面板，挂上全局句柄并启动命令循环；返回 UI 的视图。
pub fn open<V: Render>(
    cx: &mut App,
    text_scale: f64,
    build: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
) -> anyhow::Result<Entity<V>> {
    let layout = crate::core_host::core(cx)
        .map(|core| core.settings().clipboard.display.layout)
        .unwrap_or_default();
    let events = cx.new(|_| PanelEvents);
    let (commands, receiver) = async_channel::unbounded();
    cx.set_global(Panel {
        events: events.clone(),
        window: None,
        commands: commands.clone(),
    });

    let mut native = None;
    let mut content = None;
    let (window, root) = crate::platform::open_window(window_options(layout), cx, |window, cx| {
        native = Some(NativePanel::attach(window, text_scale, layout));
        let view = build(window, cx);
        content = Some(view.clone());
        let focus = cx.focus_handle();
        cx.new(|_| PanelRoot {
            content: view.into(),
            focus,
        })
    })?;
    let native = native.context("the panel window was not built")??;
    let content = content.context("the panel window was not built")?;
    native.set_command_sender(commands.clone());
    // X11 面板接收真实焦点，切换到其它应用时通过 GPUI 的 FocusOut 收起。
    #[cfg(target_os = "linux")]
    {
        let commands = commands.clone();
        let xid = native.raw_handle() as u32;
        window.update(cx, |_, window, cx| {
            root.update(cx, |_, cx| {
                cx.observe_window_activation(window, move |_, window, _| {
                    // XGrabKey 的临时抓取也产生 FocusOut；只有真实焦点已移走才隐藏。
                    if !window.is_window_active()
                        && kwikpaste_os::linux::x11::focused_window() != Some(xid)
                    {
                        let _ = commands.try_send(PanelCommand::Hide(Trigger::now(
                            TriggerSource::OutsideClick,
                        )));
                    }
                })
                .detach();
            });
        })?;
    }

    cx.global_mut::<Panel>().window = Some(window);
    let parts = Parts {
        native,
        window,
        root,
        events,
    };
    cx.spawn(async move |cx| run(parts, receiver, cx).await)
        .detach();

    Ok(content)
}

/// 命令循环持有的面板部件。
struct Parts {
    native: NativePanel,
    window: AnyWindowHandle,
    root: Entity<PanelRoot>,
    events: Entity<PanelEvents>,
}

impl Parts {
    fn emit(&self, event: PanelEvent, cx: &mut AsyncApp) {
        cx.update(|cx| self.events.update(cx, |_, cx| cx.emit(event)));
    }
}

async fn run(parts: Parts, commands: Receiver<PanelCommand>, cx: &mut AsyncApp) {
    if let Err(err) = parts.native.install() {
        log::error!("panel native setup failed: {err:#}");
    }
    if let Some(material) = cx.update(|cx| cx.try_global::<WindowMaterial>().copied()) {
        apply_material(&parts, material, cx);
    }
    probe::ready(&parts.native);
    let mut hide_on_outside_click = true;

    while let Ok(command) = commands.recv().await {
        let visible = parts.native.is_visible();
        let (want_visible, trigger) = match command {
            PanelCommand::Toggle(trigger) => {
                let summon = matches!(
                    trigger.source,
                    TriggerSource::Hotkey | TriggerSource::WinV | TriggerSource::MouseButton
                );
                if parts.native.should_recapture_on_toggle(visible, summon) {
                    parts.native.raise();
                    parts.native.set_input_capture(true);
                    continue;
                }
                (!visible, trigger)
            }
            PanelCommand::Show(trigger) => (true, trigger),
            PanelCommand::Hide(trigger)
                if !hide_on_outside_click
                    && matches!(trigger.source, TriggerSource::OutsideClick) =>
            {
                parts.native.set_input_capture(false);
                parts.emit(PanelEvent::PopupDismissed, cx);
                continue;
            }
            PanelCommand::Hide(trigger) => (false, trigger),
            PanelCommand::SetHideOnOutsideClick(hide) => {
                hide_on_outside_click = hide;
                continue;
            }
            PanelCommand::SetInputCapture(captured) => {
                parts.native.set_input_capture(captured);
                continue;
            }
            PanelCommand::DismissPopup => {
                parts.emit(PanelEvent::PopupDismissed, cx);
                continue;
            }
            PanelCommand::BeginEditing(trigger) => {
                begin_editing(&parts, trigger, cx);
                continue;
            }
            PanelCommand::EndEditing => {
                end_editing(&parts, cx);
                continue;
            }
            PanelCommand::SetTextScale(text_scale) => {
                parts.native.set_text_scale(text_scale);
                continue;
            }
            PanelCommand::SetLayout(layout) => {
                let saved = cx.update(|cx| window_state::layout(cx).saved);
                if let Some(geometry) = parts.native.set_layout(layout, saved) {
                    cx.update(|cx| window_state::save(cx, geometry));
                }
                continue;
            }
            PanelCommand::SetMaterial(material) => {
                apply_material(&parts, material, cx);
                continue;
            }
            PanelCommand::YieldForPaste { keep_visible, done } => {
                if visible {
                    end_editing(&parts, cx);
                    if !keep_visible {
                        hide(&parts, Trigger::now(TriggerSource::Paste), cx);
                    } else {
                        parts.native.set_input_capture(false);
                    }
                }
                let _ = done.try_send(visible);
                continue;
            }
        };

        match (want_visible, visible) {
            (true, false) => show(&parts, trigger, cx),
            (true, true) => parts.native.raise(),
            (false, true) => hide(&parts, trigger, cx),
            (false, false) => {}
        }
    }
}

fn apply_material(parts: &Parts, material: WindowMaterial, cx: &mut AsyncApp) {
    let result = cx.update(|cx| {
        parts.window.update(cx, |_, window, cx| {
            super::material::apply_to_window(window, &material);
            kwikpaste_ui::set_root_translucent(
                window,
                material.effective != kwikpaste_core::settings::Material::Default,
                cx,
            );
        })
    });
    if let Err(err) = result {
        log::debug!("window material not applied now: {err:#}");
    }
    #[cfg(target_os = "macos")]
    if let Err(err) = parts.native.set_material(material.effective) {
        log::debug!("macOS material not applied now: {err:#}");
    }
}

fn show(parts: &Parts, trigger: Trigger, cx: &mut AsyncApp) {
    // vsync 线程死了、或 GPU 设备恢复不了，面板只会出一帧或黑屏：不显示，有序重启。
    if !super::watchdog::render_ok("panel show") {
        return;
    }
    let native = &parts.native;
    let layout = cx.update(|cx| window_state::layout(cx));
    let placement = native
        .place(&layout)
        .inspect_err(|err| log::error!("panel placement failed, showing in place: {err:#}"))
        .ok();

    let prepared = cx.update(|cx| {
        parts.events.update(cx, |_, cx| cx.emit(PanelEvent::Shown));
        parts.window.update(cx, |_, window, cx| {
            if window.focused(cx).is_none() {
                let focus = parts.root.read(cx).focus.clone();
                window.focus(&focus, cx);
            }
            window.refresh();
        })
    });
    if let Err(err) = prepared {
        log::error!("panel could not prepare its first frame: {err:#}");
    }

    let first_frame = arm_first_frame();
    let show_started = clock::now_ticks();
    native.show();
    if crate::selftest::enabled(crate::selftest::PANEL_INVARIANTS)
        && let Err(err) = probe::panel_invariants(native)
    {
        log::error!("panel invariants selftest failed: {err:#}");
        std::process::exit(1);
    }
    SHOWN.store(true, Ordering::SeqCst);
    crate::health::set_phase(crate::health::Phase::Panel);
    let show_returned = clock::now_ticks();
    // 列表自测（跑分、截图、交互脚本）期间不装全局键鼠钩子，免得吞掉本机其它程序（包括同时在跑的
    // 平台探针）的方向键、回车和外部点击；它们的按键由自测自己派发给面板窗口。
    if !crate::selftest::list_selftest() {
        native.start_hooks();
    }
    if let Some(placement) = &placement {
        native.verify(placement);
    }

    let report = ShowReport {
        trigger,
        show_started,
        show_returned,
        native_fields: probe::enabled().then(|| native.probe_fields(placement.as_ref())),
    };
    cx.foreground_executor()
        .spawn(async move {
            if let Ok(rendered) = first_frame.recv().await {
                report.finish(rendered);
            }
        })
        .detach();
}

fn hide(parts: &Parts, trigger: Trigger, cx: &mut AsyncApp) {
    let native = &parts.native;
    let was_editing = native.is_editing();
    // 编辑中隐藏：系统自己把前台交还给原窗口，不再去抢。
    native.end_editing(false);
    native.stop_hooks();
    let geometry = native.hide();
    SHOWN.store(false, Ordering::SeqCst);
    crate::health::set_phase(crate::health::Phase::Idle);

    cx.update(|cx| {
        if let Some(geometry) = geometry {
            window_state::save(cx, geometry);
        }
        // 钩子停了，之后真实的 Ctrl 松开收不到，先把修饰键状态复位，免得快捷键提示残留。
        let _ = parts.window.update(cx, |_, window, cx| {
            window.dispatch_event(
                PlatformInput::ModifiersChanged(ModifiersChangedEvent {
                    modifiers: Modifiers::default(),
                    capslock: Capslock::default(),
                }),
                cx,
            );
        });
        parts.events.update(cx, |_, cx| {
            if was_editing {
                cx.emit(PanelEvent::EditingEnded);
            }
            cx.emit(PanelEvent::Hidden);
        });
    });

    if probe::enabled() {
        if was_editing {
            probe::editing_ended(&native.probe_fields(None));
        }
        probe::hidden(trigger, &native.probe_fields(None));
    }
}

fn begin_editing(parts: &Parts, trigger: EditTrigger, cx: &mut AsyncApp) {
    let native = &parts.native;
    if native.is_editing() {
        parts.emit(PanelEvent::EditingStarted, cx);
        return;
    }

    let result = if native.is_visible() {
        native.begin_editing(trigger)
    } else {
        Err(anyhow::anyhow!("the panel is hidden"))
    };
    let event = match &result {
        Ok(report) => {
            log::debug!(
                "editing started by {} in {:.1} ms (marked Alt swallowed: {})",
                trigger.name(),
                report.elapsed_ms,
                report.marked_alt_swallowed
            );
            PanelEvent::EditingStarted
        }
        Err(err) => {
            log::warn!("editing refused ({}): {err:#}", trigger.name());
            PanelEvent::EditingRefused
        }
    };
    parts.emit(event, cx);

    if probe::enabled() {
        probe::editing(
            trigger,
            result.as_ref().ok().copied().unwrap_or_default(),
            result.as_ref().err().map(|err| format!("{err:#}")),
            &native.probe_fields(None),
        );
    }
}

fn end_editing(parts: &Parts, cx: &mut AsyncApp) {
    if !parts.native.is_editing() {
        return;
    }

    parts.native.end_editing(true);
    parts.emit(PanelEvent::EditingEnded, cx);

    if probe::enabled() {
        probe::editing_ended(&parts.native.probe_fields(None));
    }
}

/// 一次显示从触发到首帧的计时。
struct ShowReport {
    trigger: Trigger,
    show_started: i64,
    show_returned: i64,
    native_fields: Option<String>,
}

impl ShowReport {
    /// `rendered` 是首帧渲染时刻。首帧在 `ShowWindow` 里同步画完时，它返回就已经呈现，
    /// 以返回时刻为准；否则以渲染时刻为准（之后紧跟着呈现，差不到 1 ms）。
    fn finish(self, rendered: i64) {
        let inside_show = rendered <= self.show_returned;
        let frame = if inside_show {
            self.show_returned
        } else {
            rendered
        };
        let latency = clock::ticks_to_ms(frame - self.trigger.ticks);
        log::debug!(
            "panel shown by {} in {latency:.1} ms (first frame {} the native show)",
            self.trigger.source.name(),
            if inside_show { "inside" } else { "after" }
        );

        if let Some(native_fields) = self.native_fields {
            probe::shown(
                self.trigger,
                [self.show_started, self.show_returned, rendered, frame],
                latency,
                inside_show,
                &native_fields,
            );
        }
    }
}
