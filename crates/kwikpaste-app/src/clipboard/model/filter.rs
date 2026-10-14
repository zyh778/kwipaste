//! 列表的筛选条件，移植自 1.x `clipboardViewState`（范围、分类、自定义分组、搜索词）
//! 与 `Group.tsx` 的切换规则。

use std::sync::Arc;

use super::item::{ItemKind, ListItem, SubKind};

/// 范围：全部或收藏，始终有一个选中（1.x `ClipboardRange`）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Range {
    #[default]
    All,
    Favorite,
}

impl Range {
    /// Mod+Q：在全部与收藏之间切换。
    pub fn toggled(self) -> Self {
        match self {
            Self::All => Self::Favorite,
            Self::Favorite => Self::All,
        }
    }
}

/// 分组栏里的分类；链接是文本记录里识别为 URL 的子集，紧跟文本。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Text,
    Link,
    Image,
    Files,
}

/// 分组栏里分类按钮的顺序（文本、链接、图片、文件）。
pub const CATEGORIES: [Category; 4] = [
    Category::Text,
    Category::Link,
    Category::Image,
    Category::Files,
];

/// 当前视图的筛选条件。改动任何一项列表都整体重载。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ListFilter {
    pub range: Range,
    /// 分类；再次点击当前分类时取消。
    pub category: Option<Category>,
    /// 自定义分组；再次点击当前分组时取消。
    pub group_id: Option<Arc<str>>,
    /// 搜索词（已去掉首尾空白）。
    pub keyword: Arc<str>,
}

impl ListFilter {
    pub fn favorites(&self) -> bool {
        self.range == Range::Favorite
    }

    pub fn searching(&self) -> bool {
        !self.keyword.is_empty()
    }

    /// 一条新记录或被重新使用的记录可能出现在当前视图里吗（1.x `shouldRefreshCurrentGroup`）。
    /// 与 1.x 一样不看搜索词：搜索时有新记录也回到顶部刷新。
    pub fn may_include(&self, kind: Option<ItemKind>) -> bool {
        if self.group_id.is_some() || self.favorites() {
            return false;
        }

        let matches_kind = |category: Category, kind: ItemKind| match category {
            Category::Text => kind == ItemKind::Text,
            // 链接记录在库里是文本类型；这里只知道类型，宁可多刷新一次。
            Category::Link => kind == ItemKind::Text,
            // 单个图片文件的记录属于「图片」分组，这里只知道类型，宁可多刷新一次。
            Category::Image => kind == ItemKind::Image || kind == ItemKind::Files,
            Category::Files => kind == ItemKind::Files,
        };
        match (self.category, kind) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(category), Some(kind)) => matches_kind(category, kind),
        }
    }

    /// 夹具数据源的过滤：与 core 的查询语义一致（分类、收藏、分组、搜索词不区分大小写地包含在
    /// 摘要、备注或文件名里）。core 的搜索走 FTS，这里只求自测可用。
    pub fn matches(&self, item: &ListItem) -> bool {
        if self.category.is_some_and(|category| {
            !match category {
                Category::Text => item.kind == ItemKind::Text,
                // 链接是文本记录里 sub_kind 识别为 URL 的子集。
                Category::Link => {
                    item.kind == ItemKind::Text && item.sub_kind == Some(SubKind::Url)
                }
                Category::Image => item.kind == ItemKind::Image,
                Category::Files => item.kind == ItemKind::Files,
            }
        }) {
            return false;
        }
        if self.favorites() && !item.is_favorite {
            return false;
        }
        if let Some(group) = &self.group_id
            && item.group_id.as_deref() != Some(&**group)
        {
            return false;
        }
        if !self.searching() {
            return true;
        }

        let keyword = self.keyword.to_lowercase();
        let contains = |text: &str| text.to_lowercase().contains(&keyword);
        item.summary.as_deref().is_some_and(contains)
            || item.note.as_deref().is_some_and(contains)
            || item.file_rows().iter().any(|row| contains(&row.name))
    }

    /// ←/→：在固定分类序列里循环；还没选分类时从方向对应的一端进入（1.x `selectAdjacentCategory`）。
    pub fn adjacent_category(&self, forward: bool) -> Category {
        let count = CATEGORIES.len();
        let next = match self
            .category
            .and_then(|category| CATEGORIES.iter().position(|kind| *kind == category))
        {
            Some(current) if forward => (current + 1) % count,
            Some(current) => (current + count - 1) % count,
            None if forward => 0,
            None => count - 1,
        };

        CATEGORIES.get(next).copied().unwrap_or(Category::Text)
    }
}

/// Tab / Shift+Tab：在可见的自定义分组之间循环；没有选中分组时正向取第一个、反向取最后一个
/// （1.x `selectAdjacentCustomGroup`）。
pub fn adjacent_group(
    groups: &[Arc<str>],
    current: Option<&str>,
    reverse: bool,
) -> Option<Arc<str>> {
    let count = groups.len();
    if count == 0 {
        return None;
    }

    let current = current.and_then(|id| groups.iter().position(|group| &**group == id));
    let next = match (current, reverse) {
        (None, false) => 0,
        (None, true) => count - 1,
        (Some(index), false) => (index + 1) % count,
        (Some(index), true) => (index + count - 1) % count,
    };

    groups.get(next).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_cycle_from_the_matching_end() {
        let none = ListFilter::default();
        assert_eq!(none.adjacent_category(true), Category::Text);
        assert_eq!(none.adjacent_category(false), Category::Files);

        let image = ListFilter {
            category: Some(Category::Image),
            ..ListFilter::default()
        };
        assert_eq!(image.adjacent_category(true), Category::Files);
        assert_eq!(image.adjacent_category(false), Category::Link);

        let files = ListFilter {
            category: Some(Category::Files),
            ..ListFilter::default()
        };
        assert_eq!(files.adjacent_category(true), Category::Text, "wraps");
    }

    #[test]
    fn custom_groups_cycle_in_both_directions() {
        let groups: Vec<Arc<str>> = vec!["a".into(), "b".into(), "c".into()];

        assert_eq!(adjacent_group(&groups, None, false).as_deref(), Some("a"));
        assert_eq!(adjacent_group(&groups, None, true).as_deref(), Some("c"));
        assert_eq!(
            adjacent_group(&groups, Some("c"), false).as_deref(),
            Some("a")
        );
        assert_eq!(
            adjacent_group(&groups, Some("a"), true).as_deref(),
            Some("c")
        );
        assert_eq!(
            adjacent_group(&groups, Some("hidden"), false).as_deref(),
            Some("a"),
            "a group that is no longer visible counts as none"
        );
        assert_eq!(adjacent_group(&[], None, false), None);
    }

    #[test]
    fn new_records_refresh_only_views_that_may_show_them() {
        let all = ListFilter::default();
        assert!(all.may_include(Some(ItemKind::Image)));

        let text = ListFilter {
            category: Some(Category::Text),
            ..ListFilter::default()
        };
        assert!(text.may_include(Some(ItemKind::Text)));
        assert!(!text.may_include(Some(ItemKind::Files)));
        assert!(!text.may_include(None));

        let image = ListFilter {
            category: Some(Category::Image),
            ..ListFilter::default()
        };
        assert!(image.may_include(Some(ItemKind::Files)), "image files");
        assert!(!image.may_include(Some(ItemKind::Text)));

        let favorites = ListFilter {
            range: Range::Favorite,
            ..ListFilter::default()
        };
        assert!(!favorites.may_include(Some(ItemKind::Text)));

        let group = ListFilter {
            group_id: Some("g".into()),
            ..ListFilter::default()
        };
        assert!(!group.may_include(Some(ItemKind::Text)));

        let searching = ListFilter {
            keyword: "abc".into(),
            ..ListFilter::default()
        };
        assert!(searching.may_include(Some(ItemKind::Text)));
    }

    #[test]
    fn range_toggles() {
        assert_eq!(Range::All.toggled(), Range::Favorite);
        assert_eq!(Range::Favorite.toggled(), Range::All);
    }
}
