// Copied from ratatui-widgets 0.3.0 (`ratatui-widgets/src/list/item.rs`), MIT License,
// Copyright (c) 2016-2022 Florian Dehau, Copyright (c) 2023-2025 The Ratatui
// Developers. See LICENSE and FORK.md in this crate for the changes made.

use ratatui_core::style::Style;
use ratatui_core::text::Text;

/// A single item in a [`List`]
///
/// The item's height is defined by the number of lines it contains. This can be queried using
/// [`ListItem::height`]. Similarly, [`ListItem::width`] will return the maximum width of all
/// lines.
///
/// You can set the style of an item with [`ListItem::style`] or using the [`Stylize`] trait.
/// This [`Style`] will be combined with the [`Style`] of the inner [`Text`]. The [`Style`]
/// of the [`Text`] will be added to the [`Style`] of the [`ListItem`].
///
/// You can also align a `ListItem` by aligning its underlying [`Text`] and [`Line`]s. For that,
/// see [`Text::alignment`] and [`Line::alignment`]. On a multiline `Text`, one `Line` can override
/// the alignment by setting it explicitly.
///
/// # Examples
///
/// You can create [`ListItem`]s from simple `&str`
///
/// ```rust
/// use ratatui::widgets::ListItem;
/// let item = ListItem::new("Item 1");
/// ```
///
/// Anything that can be converted to [`Text`] can be a [`ListItem`].
///
/// ```rust
/// use ratatui::text::Line;
/// use ratatui::widgets::ListItem;
///
/// let item1: ListItem = "Item 1".into();
/// let item2: ListItem = Line::raw("Item 2").into();
/// ```
///
/// A [`ListItem`] styled with [`Stylize`]
///
/// ```rust
/// use ratatui::style::Stylize;
/// use ratatui::widgets::ListItem;
///
/// let item = ListItem::new("Item 1").red().on_white();
/// ```
///
/// If you need more control over the item's style, you can explicitly style the underlying
/// [`Text`]
///
/// ```rust
/// use ratatui::style::Stylize;
/// use ratatui::text::{Span, Text};
/// use ratatui::widgets::ListItem;
///
/// let mut text = Text::default();
/// text.extend(["Item".blue(), Span::raw(" "), "1".bold().red()]);
/// let item = ListItem::new(text);
/// ```
///
/// A right-aligned `ListItem`
///
/// ```rust
/// use ratatui::text::Text;
/// use ratatui::widgets::ListItem;
///
/// ListItem::new(Text::from("foo").right_aligned());
/// ```
///
/// [`List`]: crate::list::List
/// [`Stylize`]: ratatui_core::style::Stylize
/// [`Line`]: ratatui_core::text::Line
/// [`Line::alignment`]: ratatui_core::text::Line::alignment
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct ListItem<'a> {
    pub(crate) content: Text<'a>,
    pub(crate) style: Style,
}

impl<'a> ListItem<'a> {
    /// Creates a new [`ListItem`]
    ///
    /// The `content` parameter accepts any value that can be converted into [`Text`].
    ///
    /// # Examples
    ///
    /// You can create [`ListItem`]s from simple `&str`
    ///
    /// ```rust
    /// use ratatui::widgets::ListItem;
    ///
    /// let item = ListItem::new("Item 1");
    /// ```
    ///
    /// Anything that can be converted to [`Text`] can be a [`ListItem`].
    ///
    /// ```rust
    /// use ratatui::text::Line;
    /// use ratatui::widgets::ListItem;
    ///
    /// let item1: ListItem = "Item 1".into();
    /// let item2: ListItem = Line::raw("Item 2").into();
    /// ```
    ///
    /// You can also create multiline items
    ///
    /// ```rust
    /// use ratatui::widgets::ListItem;
    ///
    /// let item = ListItem::new("Multi-line\nitem");
    /// ```
    ///
    /// # See also
    ///
    /// - [`List::new`](super::List::new) to create a list of items that can be converted to
    ///   [`ListItem`]
    pub fn new<T>(content: T) -> Self
    where
        T: Into<Text<'a>>,
    {
        Self {
            content: content.into(),
            style: Style::default(),
        }
    }

    /// Sets the item style
    ///
    /// `style` accepts any type that is convertible to [`Style`] (e.g. [`Style`], [`Color`], or
    /// your own type that implements [`Into<Style>`]).
    ///
    /// This [`Style`] can be overridden by the [`Style`] of the [`Text`] content.
    ///
    /// This is a fluent setter method which must be chained or used as it consumes self
    ///
    /// # Example
    ///
    /// ```rust
    /// use ratatui::style::{Style, Stylize};
    /// use ratatui::widgets::ListItem;
    ///
    /// let item = ListItem::new("Item 1").style(Style::new().red().italic());
    /// ```
    ///
    /// `ListItem` also implements the [`Styled`] trait, which means you can use style shorthands
    /// from the [`Stylize`](ratatui_core::style::Stylize) trait to set the style of the widget more
    /// concisely.
    ///
    /// ```rust
    /// use ratatui::style::Stylize;
    /// use ratatui::widgets::ListItem;
    ///
    /// let item = ListItem::new("Item 1").red().italic();
    /// ```
    ///
    /// [`Styled`]: ratatui_core::style::Styled
    /// [`ListState`]: crate::list::ListState
    /// [`Color`]: ratatui_core::style::Color
    #[must_use = "method moves the value of self and returns the modified value"]
    pub fn style<S: Into<Style>>(mut self, style: S) -> Self {
        self.style = style.into();
        self
    }

    /// Returns the item height
    ///
    /// # Examples
    ///
    /// One line item
    ///
    /// ```rust
    /// use ratatui::widgets::ListItem;
    ///
    /// let item = ListItem::new("Item 1");
    /// assert_eq!(item.height(), 1);
    /// ```
    ///
    /// Two lines item (note the `\n`)
    ///
    /// ```rust
    /// use ratatui::widgets::ListItem;
    ///
    /// let item = ListItem::new("Multi-line\nitem");
    /// assert_eq!(item.height(), 2);
    /// ```
    pub fn height(&self) -> usize {
        self.content.height()
    }

    /// Returns the max width of all the lines
    ///
    /// # Examples
    ///
    /// ```rust
    /// use ratatui::widgets::ListItem;
    ///
    /// let item = ListItem::new("12345");
    /// assert_eq!(item.width(), 5);
    /// ```
    ///
    /// ```rust
    /// use ratatui::widgets::ListItem;
    ///
    /// let item = ListItem::new("12345\n1234567");
    /// assert_eq!(item.width(), 7);
    /// ```
    pub fn width(&self) -> usize {
        self.content.width()
    }
}

impl<'a, T> From<T> for ListItem<'a>
where
    T: Into<Text<'a>>,
{
    fn from(value: T) -> Self {
        Self::new(value)
    }
}
