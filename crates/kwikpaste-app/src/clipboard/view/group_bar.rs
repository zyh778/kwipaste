//! 头部下方的分组栏（1.x `Group.tsx`）：范围（全部 / 收藏）、分类（文本 / 图片 / 文件）和自定义分组。
//!
//! - 范围始终有一个选中；分类、自定义分组再点一次取消；
//! - 隐藏的分组不出现在栏上；放不下的自定义分组收进“更多”菜单；
//! - 自定义分组右键可以编辑、隐藏、删除；新增、编辑、管理分组的弹框由主窗口打开（`group_dialogs`）；
//! - 双栏布局拆成两条：左栏只有分类，右栏是「收藏」加自定义分组的页签（始终有一个选中）。

use std::sync::Arc;

use gpui::{
    AnyElement, Context, EventEmitter, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, Role, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    prelude::FluentBuilder as _,
};
use kwikpaste_ui::{
    Glyph, IconButtonLook as Look, IconName, MenuEntry, MenuItem, MenuTrigger, context_menu,
    group_icon_path, icon_button, separator, theme::space,
};

use crate::{
    clipboard::{
        model::filter::{CATEGORIES, Category, ListFilter, Range},
        source::Group,
    },
    i18n::t,
    platform::window_drag::WindowDragArea as _,
};

/// 1.x 分组按钮的尺寸与间距（设计 px），用来算一行放得下几个自定义分组。
const BUTTON: f32 = 24.;
const GAP: f32 = 4.;
/// 第二条分隔线右边缘到分组栏左边缘的距离（左内边距 + 范围 + 分类 + 两条分隔线，见 1.x
/// `computeCustomGroupCapacity`），再加分隔线外边距和一个间距。分类比 1.x 多一个「链接」按钮。
const CUSTOM_START: f32 = 170. + BUTTON + GAP + 4. + GAP;
/// 末尾留给“更多”或“新增”按钮的位置。
const ACTION_SLOT: f32 = GAP + BUTTON;
/// 右栏页签里自定义分组的起点：左内边距 + 收藏按钮 + 分隔线，再加分隔线外边距和一个间距。
const SIDE_START: f32 = 12. + BUTTON + GAP + 5. + 4. + GAP;

/// 分组栏放哪些按钮。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarMode {
    /// 单栏：范围、分类、自定义分组。
    Full,
    /// 双栏的左栏：只有分类。
    Categories,
    /// 双栏的右栏：收藏和自定义分组。
    Side,
}

/// 分组栏发给主窗口的事件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupBarEvent {
    SelectRange(Range),
    ToggleCategory(Category),
    ToggleGroup(Arc<str>),
    EditGroup(Group),
    HideGroup(Group),
    DeleteGroup(Group),
    NewGroup,
    ManageGroups,
}

pub struct GroupBar {
    mode: BarMode,
    groups: Vec<Group>,
    range: Range,
    category: Option<Category>,
    group_id: Option<Arc<str>>,
    hints: bool,
}

impl EventEmitter<GroupBarEvent> for GroupBar {}

/// 选中项胶囊里名称的最大宽度（同 [`super::chrome`] 里的 `max_w`）。
const CHIP_LABEL_MAX: f32 = 72.;

/// 一行能放下几个自定义分组（宽度为设计 px，`start` 是第一个自定义分组的起点）。
fn capacity_from(width: f32, start: f32) -> usize {
    let available = (width - start - ACTION_SLOT).max(0.);
    ((available + GAP) / (BUTTON + GAP)).floor().max(0.) as usize
}

/// 选中项是带名称的胶囊（左右各 8、图标 16、间距 4 再加名称），比普通按钮宽出的部分（设计 px）。
/// 名称宽度按 12 px 字估：全角字 12、其余 7，宁可多算，不能让末尾的按钮被裁掉。
fn chip_extra(label: &str) -> f32 {
    let text: f32 = label
        .chars()
        .map(|c| if u32::from(c) >= 0x2E80 { 12. } else { 7. })
        .sum();

    36. + text.min(CHIP_LABEL_MAX) - BUTTON
}

impl GroupBar {
    pub fn new(mode: BarMode) -> Self {
        Self {
            mode,
            groups: Vec::new(),
            range: Range::All,
            category: None,
            group_id: None,
            hints: false,
        }
    }

    pub fn set_mode(&mut self, mode: BarMode, cx: &mut Context<Self>) {
        if self.mode != mode {
            self.mode = mode;
            cx.notify();
        }
    }

    pub fn set_groups(&mut self, groups: Vec<Group>, cx: &mut Context<Self>) {
        self.groups = groups;
        cx.notify();
    }

    /// 分组栏上可见的自定义分组的 id（Tab 循环用）。
    pub fn visible_ids(&self) -> Vec<Arc<str>> {
        self.groups
            .iter()
            .filter(|group| !group.is_hidden)
            .map(|group| group.id.clone())
            .collect()
    }

    pub fn set_filter(&mut self, filter: &ListFilter, cx: &mut Context<Self>) {
        self.range = filter.range;
        self.category = filter.category;
        self.group_id = filter.group_id.clone();
        cx.notify();
    }

    pub fn set_hints(&mut self, hints: bool, cx: &mut Context<Self>) {
        if self.hints != hints {
            self.hints = hints;
            cx.notify();
        }
    }

    fn range_button(&self, range: Range, cx: &mut Context<Self>) -> AnyElement {
        let (id, icon, label) = match range {
            Range::All => (
                "group-range-all",
                IconName::GroupAll,
                t("clipboard:groups.all"),
            ),
            Range::Favorite => (
                "group-range-favorite",
                IconName::GroupFavorite,
                t("clipboard:groups.favorite"),
            ),
        };
        let (selected, hint) = if self.mode == BarMode::Side {
            (self.group_id.is_none(), None)
        } else {
            // Mod+Q 的角标标在按下去会切到的那个按钮上。
            let hint = self.hints && self.range.toggled() == range;
            (self.range == range, hint.then(|| SharedString::from("Q")))
        };

        icon_button(cx, id, Glyph::Icon(icon), label, hint, look(selected))
            .role(Role::Tab)
            .aria_selected(selected)
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(GroupBarEvent::SelectRange(range))))
            .into_any_element()
    }

    fn category_button(&self, category: Category, cx: &mut Context<Self>) -> AnyElement {
        let (id, icon, label) = match category {
            Category::Text => (
                "group-text",
                IconName::GroupText,
                t("clipboard:groups.text"),
            ),
            Category::Link => (
                "group-link",
                IconName::GroupLink,
                t("clipboard:groups.link"),
            ),
            Category::Image => (
                "group-image",
                IconName::GroupImage,
                t("clipboard:groups.image"),
            ),
            Category::Files => (
                "group-files",
                IconName::GroupFiles,
                t("clipboard:groups.files"),
            ),
        };

        icon_button(
            cx,
            id,
            Glyph::Icon(icon),
            label,
            None,
            look(self.category == Some(category)),
        )
        .on_click(
            cx.listener(move |_, _, _, cx| cx.emit(GroupBarEvent::ToggleCategory(category))),
        )
        .role(Role::Tab)
        .aria_selected(self.category == Some(category))
        .into_any_element()
    }

    fn custom_button(&self, group: &Group, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.group_id.as_deref() == Some(&*group.id);
        let id = group.id.clone();
        let entity = cx.entity().downgrade();
        let menu_group = group.clone();

        let button = icon_button(
            cx,
            SharedString::from(format!("group-custom-{}", group.id)),
            Glyph::Path(group_icon_path(&group.icon)),
            SharedString::from(group.name.to_string()),
            None,
            look(selected),
        )
        .role(Role::Tab)
        .aria_selected(selected)
        .on_click(cx.listener(move |_, _, _, cx| {
            cx.emit(GroupBarEvent::ToggleGroup(id.clone()));
        }));

        context_menu(button, move |_, _| group_menu(&menu_group, entity.clone()))
    }

    fn more_button(&self, overflow: Vec<Group>, cx: &mut Context<Self>) -> AnyElement {
        let selected = overflow
            .iter()
            .any(|group| self.group_id.as_deref() == Some(&*group.id));
        let current = self.group_id.clone();
        let entity = cx.entity().downgrade();
        let hint = self.hints.then(|| SharedString::from("N"));

        MenuTrigger::new("group-more")
            .child(icon_button(
                cx,
                "group-more-button",
                Glyph::Icon(IconName::Ellipsis),
                t("clipboard:groups.more"),
                hint,
                look(selected),
            ))
            .dropdown(move |_, _| {
                let mut entries: Vec<MenuEntry> = overflow
                    .iter()
                    .map(|group| {
                        let id = group.id.clone();
                        let entity = entity.clone();
                        MenuItem::new(SharedString::from(group.name.to_string()), move |_, cx| {
                            entity
                                .update(cx, |_, cx| {
                                    cx.emit(GroupBarEvent::ToggleGroup(id.clone()));
                                })
                                .ok();
                        })
                        .icon_path(group_icon_path(&group.icon))
                        .checked(current.as_deref() == Some(&*group.id))
                        .into()
                    })
                    .collect();
                entries.push(MenuEntry::Separator);
                entries.extend(create_entries(entity.clone()));
                entries
            })
    }

    fn create_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.entity().downgrade();
        let hint = self.hints.then(|| SharedString::from("N"));

        let button = icon_button(
            cx,
            "group-create",
            Glyph::Icon(IconName::Plus),
            t("clipboard:groups.add"),
            hint,
            Look::Text,
        )
        .on_click(cx.listener(|_, _, _, cx| cx.emit(GroupBarEvent::NewGroup)));

        context_menu(button, move |_, _| {
            let entity = entity.clone();
            vec![
                MenuItem::new(t("clipboard:groups.manage"), move |_, cx| {
                    entity
                        .update(cx, |_, cx| cx.emit(GroupBarEvent::ManageGroups))
                        .ok();
                })
                .icon(IconName::Settings2)
                .into(),
            ]
        })
    }
}

fn look(selected: bool) -> Look {
    if selected { Look::Chip } else { Look::Text }
}

/// 自定义分组的右键菜单：编辑、隐藏、删除（1.x `buildGroupActionMenuItems`）。
fn group_menu(group: &Group, entity: gpui::WeakEntity<GroupBar>) -> Vec<MenuEntry> {
    let edit = group.clone();
    let hide = group.clone();
    let delete = group.clone();
    let edit_entity = entity.clone();
    let hide_entity = entity.clone();

    vec![
        MenuItem::new(t("clipboard:groups.edit"), move |_, cx| {
            edit_entity
                .update(cx, |_, cx| cx.emit(GroupBarEvent::EditGroup(edit.clone())))
                .ok();
        })
        .icon(IconName::Pencil)
        .into(),
        MenuItem::new(t("clipboard:groups.hide"), move |_, cx| {
            hide_entity
                .update(cx, |_, cx| cx.emit(GroupBarEvent::HideGroup(hide.clone())))
                .ok();
        })
        .icon(IconName::EyeOff)
        .into(),
        MenuEntry::Separator,
        MenuItem::new(t("clipboard:groups.delete"), move |_, cx| {
            entity
                .update(cx, |_, cx| {
                    cx.emit(GroupBarEvent::DeleteGroup(delete.clone()))
                })
                .ok();
        })
        .icon(IconName::Trash2)
        .danger()
        .into(),
    ]
}

fn create_entries(entity: gpui::WeakEntity<GroupBar>) -> Vec<MenuEntry> {
    let manage = entity.clone();

    vec![
        MenuItem::new(t("clipboard:groups.add"), move |_, cx| {
            entity
                .update(cx, |_, cx| cx.emit(GroupBarEvent::NewGroup))
                .ok();
        })
        .icon(IconName::Plus)
        .into(),
        MenuItem::new(t("clipboard:groups.manage"), move |_, cx| {
            manage
                .update(cx, |_, cx| cx.emit(GroupBarEvent::ManageGroups))
                .ok();
        })
        .icon(IconName::Settings2)
        .into(),
    ]
}

impl Render for GroupBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut width = window.viewport_size().width.as_f32() / window.rem_size().as_f32() * 16.;
        if self.mode != BarMode::Full {
            // 双栏两列等宽，每条分组栏占一半。
            width /= 2.;
        }
        let bar = div()
            .id(match self.mode {
                BarMode::Side => "clipboard-side-group-bar",
                _ => "clipboard-group-bar",
            })
            .role(Role::TabList)
            .aria_label(t("clipboard:accessibility.groupBar"))
            .flex()
            .flex_none()
            .w_full()
            .items_center()
            .gap(space((GAP) / 4.))
            .overflow_hidden()
            .px(space((12.) / 4.))
            .pb(space((8.) / 4.));
        let drag_area = div()
            .flex_1()
            .min_w_0()
            .h(space((32.) / 4.))
            .window_drag_area();
        if self.mode == BarMode::Categories {
            let categories = CATEGORIES.map(|category| self.category_button(category, cx));
            return bar.children(categories).child(drag_area);
        }

        let visible: Vec<Group> = self
            .groups
            .iter()
            .filter(|group| !group.is_hidden)
            .cloned()
            .collect();
        let side = self.mode == BarMode::Side;
        let start = if side { SIDE_START } else { CUSTOM_START };
        // 选中的范围、分类和自定义分组都是带名称的胶囊，先扣掉它们多占的宽度再算放得下几个。
        let mut chips = if side {
            0.
        } else {
            chip_extra(&match self.range {
                Range::All => t("clipboard:groups.all"),
                Range::Favorite => t("clipboard:groups.favorite"),
            })
        };
        if side && self.group_id.is_none() {
            chips += chip_extra(&t("clipboard:groups.favorite"));
        }
        if let Some(category) = self.category.filter(|_| !side) {
            chips += chip_extra(&t(match category {
                Category::Text => "clipboard:groups.text",
                Category::Link => "clipboard:groups.link",
                Category::Image => "clipboard:groups.image",
                Category::Files => "clipboard:groups.files",
            }));
        }
        // 选中的自定义分组先按它自己的胶囊预留；这样仍放不下它时，它收进“更多”，换成“更多”的胶囊。
        let selected = self
            .group_id
            .as_ref()
            .and_then(|id| visible.iter().position(|group| group.id == *id));
        let mut fits = capacity_from(width - chips, start);
        if let Some(index) = selected {
            let with_chip = visible
                .get(index)
                .map_or(0., |group| chip_extra(&group.name));
            fits = capacity_from(width - chips - with_chip, start);
            if index >= fits {
                fits = capacity_from(
                    width - chips - chip_extra(&t("clipboard:groups.more")),
                    start,
                );
            }
        }
        let inline = visible.len().min(fits);
        let overflow: Vec<Group> = visible.iter().skip(inline).cloned().collect();

        let customs: Vec<AnyElement> = visible
            .iter()
            .take(inline)
            .map(|group| self.custom_button(group, cx))
            .collect();
        let action = if overflow.is_empty() {
            self.create_button(cx)
        } else {
            self.more_button(overflow, cx)
        };

        let bar = if side {
            bar.child(self.range_button(Range::Favorite, cx))
        } else {
            let ranges = [Range::All, Range::Favorite].map(|range| self.range_button(range, cx));
            let categories = CATEGORIES.map(|category| self.category_button(category, cx));
            bar.children(ranges)
                .child(separator(cx))
                .children(categories)
        };

        bar.child(separator(cx))
            .when(!customs.is_empty(), |bar| bar.children(customs))
            .child(action)
            .child(drag_area)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 360 px 宽的面板放 4 个自定义分组（分类比 1.x 多一个「链接」按钮，同宽少放一个）。
    #[test]
    fn the_default_panel_fits_five_custom_groups() {
        assert_eq!(capacity_from(360., CUSTOM_START), 4);
        assert_eq!(capacity_from(200., CUSTOM_START), 0);
        assert_eq!(capacity_from(500., CUSTOM_START), 9);
    }

    /// 胶囊多占的宽度：两个全角字 24、两个半角字 14，名称再长也只算到上限。
    #[test]
    fn chips_reserve_their_label_width() {
        assert_eq!(chip_extra("全部"), 36.);
        assert_eq!(chip_extra("All"), 33.);
        assert_eq!(chip_extra("一个很长很长很长很长的分组名称"), 84.);
    }
}
