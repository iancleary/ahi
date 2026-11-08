// Copied from ratatui-widgets 0.3.0 (`ratatui-widgets/src/block.rs`), MIT License,
// Copyright (c) 2016-2022 Florian Dehau, Copyright (c) 2023-2025 The Ratatui
// Developers. See LICENSE and FORK.md in this crate for the changes made.

//! Elements related to the `Block` base widget.
//!
//! This holds everything needed to display and configure a [`Block`].
//!
//! In its simplest form, a `Block` is a [border](Borders) around another widget. It can have a
//! [title](Block::title) and [padding](Block::padding).

use std::vec::Vec;

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Alignment, Rect};
use ratatui_core::style::{Style, Styled};
use ratatui_core::symbols::border;
use ratatui_core::symbols::merge::MergeStrategy;
use ratatui_core::text::Line;
use ratatui_core::widgets::Widget;

pub use self::padding::Padding;
use crate::borders::{BorderType, Borders};

mod padding;

/// A widget that renders borders, titles, and padding around other widgets.
///
/// A `Block` is a foundational widget that creates visual containers by drawing borders around an
/// area. It serves as a wrapper or frame for other widgets, providing structure and visual
/// separation in terminal UIs. Most built-in widgets in Ratatui use a pattern where they accept an
/// optional `Block` parameter that wraps the widget's content.
///
/// When a widget renders with a block, the widget's style is applied first, then the block's style,
/// and finally the widget's content is rendered within the inner area calculated by the block. This
/// layered approach allows for flexible styling where the block can provide background colors,
/// borders, and padding while the inner widget handles its own content styling.
///
/// Multiple blocks can be nested within each other. The [`Block::inner`] method calculates the area
/// available for content after accounting for borders, titles, and padding, making it easy to nest
/// blocks or position widgets within a block's boundaries.
///
/// # Constructor Methods
///
/// - [`Block::new`] - Creates a block with no borders or padding
/// - [`Block::bordered`] - Creates a block with all borders enabled
///
/// # Border Configuration
///
/// - [`Block::borders`] - Specifies which borders to display
/// - [`Block::border_style`] - Sets the style of the borders
/// - [`Block::border_type`] - Sets border symbols (single, double, thick, rounded, etc.)
/// - [`Block::border_set`] - Sets custom border symbols as a [`border::Set`]
/// - [`Block::merge_borders`] - Controls how borders merge with adjacent blocks
///
/// # Title Configuration
///
/// - [`Block::title`] - Adds a title to the block
/// - [`Block::title_top`] - Adds a title to the top of the block
/// - [`Block::title_bottom`] - Adds a title to the bottom of the block
/// - [`Block::title_alignment`] - Sets default alignment for all titles
/// - [`Block::title_style`] - Sets the style for all titles
/// - [`Block::title_position`] - Sets default position for titles
///
/// # Styling and Layout
///
/// - [`Block::style`] - Sets the base style of the block
/// - [`Block::padding`] - Adds internal padding within the borders
/// - [`Block::inner`] - Calculates the inner area available for content
///
/// # Title Behavior
///
/// You can add multiple titles to a block, and they will be rendered with spaces separating titles
/// that share the same position or alignment. When both centered and non-centered titles exist, the
/// centered space is calculated based on the full width of the block.
///
/// Titles are set using the `.title`, `.title_top`, and `.title_bottom` methods. These methods
/// accept a string or any type that can be converted into a [`Line`], such as a string slice,
/// `String`, or a vector of [`Span`]s. To control the alignment of a title (left, center, right),
/// pass a `Line` with the desired alignment, e.g. `Line::from("Title").centered()`.
///
/// By default, `.title` places the title at the top of the block, but you can use `.title_top` or
/// `.title_bottom` to explicitly set the position. The default alignment for all titles can be set
/// with [`Block::title_alignment`], and the default position for all titles can be set with
/// [`Block::title_position`].
///
/// Note that prior to `v0.30.0`, the `block::Title` struct was used to create titles. This struct
/// has been removed. The new recommended approach is to use [`Line`] with a specific alignment for
/// the title's content and the [`Block::title_top`] and [`Block::title_bottom`] methods for
/// positioning.
///
/// Titles avoid being rendered in corners when borders are present, but will align to edges when no
/// border exists on that side:
///
/// ```plain
/// ┌With at least a left border───
///
/// Without left border───
/// ```
///
/// # Nesting Widgets with `inner`
///
/// The [`Block::inner`] method computes the area inside the block after accounting for borders,
/// titles, and padding. This allows you to nest widgets inside a block by rendering the block
/// first, then rendering other widgets in the returned inner area.
///
/// For example, you can nest a block inside another block:
///
/// ```
/// use ratatui::Frame;
/// use ratatui::widgets::Block;
///
/// # fn render_nested_block(frame: &mut Frame) {
/// let outer_block = Block::bordered().title("Outer");
/// let inner_block = Block::bordered().title("Inner");
///
/// let outer_area = frame.area();
/// let inner_area = outer_block.inner(outer_area);
///
/// frame.render_widget(outer_block, outer_area);
/// frame.render_widget(inner_block, inner_area);
/// # }
/// ```
///
/// You can also use the standard [`Layout`] functionality to further subdivide the inner area and
/// lay out multiple widgets inside a block.
///
/// # Integration with Other Widgets
///
/// Most widgets in Ratatui accept a block parameter. For example, [`Paragraph`], [`List`],
/// `Table`, and other widgets can be wrapped with a block:
///
/// ```
/// use ratatui::widgets::{Block, Paragraph};
///
/// let paragraph = Paragraph::new("Hello, world!").block(Block::bordered().title("My Paragraph"));
/// ```
///
/// This pattern allows widgets to focus on their content while blocks handle the visual framing.
///
/// # Styling
///
/// Styles are applied in a specific order: first the block's base style, then border styles, then
/// title styles, and finally any content widget styles. This layered approach allows for flexible
/// styling where outer styles provide defaults that inner styles can override.
///
/// `Block` implements [`Stylize`](ratatui_core::style::Stylize), allowing you to use style
/// shorthand methods:
///
/// ```
/// use ratatui::style::Stylize;
/// use ratatui::widgets::Block;
///
/// let block = Block::bordered().red().on_white().bold();
/// ```
///
/// # Examples
///
/// Create a simple bordered block:
///
/// ```
/// use ratatui::widgets::Block;
///
/// let block = Block::bordered().title("My Block");
/// ```
///
/// Create a block with custom border styling:
///
/// ```
/// use ratatui::style::{Color, Style, Stylize};
/// use ratatui::widgets::{Block, BorderType};
///
/// let block = Block::bordered()
///     .title("Styled Block")
///     .border_type(BorderType::Rounded)
///     .border_style(Style::new().cyan())
///     .style(Style::new().on_black());
/// ```
///
/// Use a block to wrap another widget:
///
/// ```
/// use ratatui::widgets::{Block, Paragraph};
///
/// let paragraph = Paragraph::new("Hello, world!").block(Block::bordered().title("Greeting"));
/// ```
///
/// Add multiple titles with different alignments:
///
/// ```
/// use ratatui::text::Line;
/// use ratatui::widgets::Block;
///
/// let block = Block::bordered()
///     .title_top(Line::from("Left").left_aligned())
///     .title_top(Line::from("Center").centered())
///     .title_top(Line::from("Right").right_aligned())
///     .title_bottom("Status: OK");
/// ```
///
/// # See Also
///
/// - [Block recipe] - Visual examples and common patterns (on the ratatui website)
/// - [Collapse borders recipe] - Techniques for creating seamless layouts (on the ratatui website)
/// - [`MergeStrategy`] - Controls how borders merge with adjacent elements
///
/// [Block recipe]: https://ratatui.rs/recipes/widgets/block/
/// [Collapse borders recipe]: https://ratatui.rs/recipes/layout/collapse-borders/
/// [`Paragraph`]: crate::paragraph::Paragraph
/// [`Span`]: ratatui_core::text::Span
/// [`Stylize`]: ratatui_core::style::Stylize
/// [`List`]: crate::list::List
/// [`Layout`]: ratatui_core::layout::Layout
#[derive(Debug, Default, Clone, Eq, PartialEq, Hash)]
pub struct Block<'a> {
    /// List of titles
    titles: Vec<(Option<TitlePosition>, Line<'a>)>,
    /// The style to be patched to all titles of the block
    titles_style: Style,
    /// The default alignment of the titles that don't have one
    titles_alignment: Alignment,
    /// The default position of the titles that don't have one
    titles_position: TitlePosition,
    /// Visible borders
    borders: Borders,
    /// Border style
    border_style: Style,
    /// The symbols used to render the border. The default is plain lines but one can choose to
    /// have rounded or doubled lines instead or a custom set of symbols
    border_set: border::Set<'a>,
    /// Widget style
    style: Style,
    /// Block padding
    padding: Padding,
    /// Border merging strategy
    merge_borders: MergeStrategy,
}

/// Defines the position of the title.
///
/// The title can be positioned on top or at the bottom of the block.
///
/// # Example
///
/// ```
/// use ratatui::widgets::{Block, TitlePosition};
///
/// Block::bordered()
///     .title_position(TitlePosition::Top)
///     .title("Top Title");
/// Block::bordered()
///     .title_position(TitlePosition::Bottom)
///     .title("Bottom Title");
/// ```
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TitlePosition {
    /// Position the title at the top of the block.
    #[default]
    Top,
    /// Position the title at the bottom of the block.
    Bottom,
}

impl<'a> Block<'a> {
    /// Creates a new block with no [`Borders`] or [`Padding`].
    pub const fn new() -> Self {
        Self {
            titles: Vec::new(),
            titles_style: Style::new(),
            titles_alignment: Alignment::Left,
            titles_position: TitlePosition::Top,
            borders: Borders::NONE,
            border_style: Style::new(),
            border_set: BorderType::Plain.to_border_set(),
            style: Style::new(),
            padding: Padding::ZERO,
            merge_borders: MergeStrategy::Replace,
        }
    }

    /// Create a new block with [all borders](Borders::ALL) shown
    ///
    /// ```
    /// use ratatui::widgets::{Block, Borders};
    ///
    /// assert_eq!(Block::bordered(), Block::new().borders(Borders::ALL));
    /// ```
    pub const fn bordered() -> Self {
        let mut block = Self::new();
        block.borders = Borders::ALL;
        block
    }

    /// Adds a title to the block using the default position.
    ///
    /// The position of the title is determined by the `title_position` field of the block, which
    /// defaults to `Top`. This can be changed using the [`Block::title_position`] method. For
    /// explicit positioning, use [`Block::title_top`] or [`Block::title_bottom`].
    ///
    /// The `title` function allows you to add a title to the block. You can call this function
    /// multiple times to add multiple titles.
    ///
    /// Each title will be rendered with a single space separating titles that are in the same
    /// position or alignment. When both centered and non-centered titles are rendered, the centered
    /// space is calculated based on the full width of the block, rather than the leftover width.
    ///
    /// You can provide any type that can be converted into [`Line`] including: strings, string
    /// slices (`&str`), borrowed strings (`Cow<str>`), [spans](ratatui_core::text::Span), or
    /// vectors of [spans](ratatui_core::text::Span) (`Vec<Span>`).
    ///
    /// By default, the titles will avoid being rendered in the corners of the block but will align
    /// against the left or right edge of the block if there is no border on that edge. The
    /// following demonstrates this behavior, notice the second title is one character off to the
    /// left.
    ///
    /// ```plain
    /// ┌With at least a left border───
    ///
    /// Without left border───
    /// ```
    ///
    /// Note: If the block is too small and multiple titles overlap, the border might get cut off at
    /// a corner.
    ///
    /// # Examples
    ///
    /// See the [Block example] for a visual representation of how the various borders and styles
    /// look when rendered.
    ///
    /// The following example demonstrates:
    /// - Default title alignment
    /// - Multiple titles (notice "Center" is centered according to the full with of the block, not
    ///   the leftover space)
    /// - Two titles with the same alignment (notice the left titles are separated)
    /// ```
    /// use ratatui::text::Line;
    /// use ratatui::widgets::Block;
    ///
    /// Block::bordered()
    ///     .title("Title")
    ///     .title(Line::from("Left").left_aligned())
    ///     .title(Line::from("Right").right_aligned())
    ///     .title(Line::from("Center").centered());
    /// ```
    ///
    /// # See also
    ///
    /// Titles attached to a block can have default behaviors. See
    /// - [`Block::title_style`]
    /// - [`Block::title_alignment`]
    ///
    /// # History
    ///
    /// In previous releases of Ratatui this method accepted `Into<Title>` instead of
    /// [`Into<Line>`]. We found that storing the position in the block and the alignment in the
    /// line better reflects the intended use of the block and its titles. See
    /// <https://github.com/ratatui/ratatui/issues/738> for more information.
    ///
    /// [Block example]: https://github.com/ratatui/ratatui/blob/main/examples/README.md#block
    #[must_use = "method moves the value of self and returns the modified value"]
    pub fn title<T>(mut self, title: T) -> Self
    where
        T: Into<Line<'a>>,
    {
        self.titles.push((None, title.into()));
        self
    }

    /// Adds a title to the top of the block.
    ///
    /// You can provide any type that can be converted into [`Line`] including: strings, string
    /// slices (`&str`), borrowed strings (`Cow<str>`), [spans](ratatui_core::text::Span), or
    /// vectors of [spans](ratatui_core::text::Span) (`Vec<Span>`).
    ///
    /// # Example
    ///
    /// ```
    /// use ratatui::{ widgets::Block, text::Line };
    ///
    /// Block::bordered()
    ///     .title_top("Left1") // By default in the top left corner
    ///     .title_top(Line::from("Left2").left_aligned())
    ///     .title_top(Line::from("Right").right_aligned())
    ///     .title_top(Line::from("Center").centered());
    ///
    /// // Renders
    /// // ┌Left1─Left2───Center─────────Right┐
    /// // │                                  │
    /// // └──────────────────────────────────┘
    /// ```
    #[must_use = "method moves the value of self and returns the modified value"]
    pub fn title_top<T: Into<Line<'a>>>(mut self, title: T) -> Self {
        let line = title.into();
        self.titles.push((Some(TitlePosition::Top), line));
        self
    }

    /// Adds a title to the bottom of the block.
    ///
    /// You can provide any type that can be converted into [`Line`] including: strings, string
    /// slices (`&str`), borrowed strings (`Cow<str>`), [spans](ratatui_core::text::Span), or
    /// vectors of [spans](ratatui_core::text::Span) (`Vec<Span>`).
    ///
    /// # Example
    ///
    /// ```
    /// use ratatui::{ widgets::Block, text::Line };
    ///
    /// Block::bordered()
    ///     .title_bottom("Left1") // By default in the top left corner
    ///     .title_bottom(Line::from("Left2").left_aligned())
    ///     .title_bottom(Line::from("Right").right_aligned())
    ///     .title_bottom(Line::from("Center").centered());
    ///
    /// // Renders
    /// // ┌──────────────────────────────────┐
    /// // │                                  │
    /// // └Left1─Left2───Center─────────Right┘
    /// ```
    #[must_use = "method moves the value of self and returns the modified value"]
    pub fn title_bottom<T: Into<Line<'a>>>(mut self, title: T) -> Self {
        let line = title.into();
        self.titles.push((Some(TitlePosition::Bottom), line));
        self
    }

    /// Applies the style to all titles.
    ///
    /// This style will be applied to all titles of the block. If a title has a style set, it will
    /// be applied after this style. This style will be applied after any [`Block::style`] or
    /// [`Block::border_style`] is applied.
    ///
    /// See [`Style`] for more information on how merging styles works.
    ///
    /// `style` accepts any type that is convertible to [`Style`] (e.g. [`Style`], [`Color`], or
    /// your own type that implements [`Into<Style>`]).
    ///
    /// [`Color`]: ratatui_core::style::Color
    #[must_use = "method moves the value of self and returns the modified value"]
    pub fn title_style<S: Into<Style>>(mut self, style: S) -> Self {
        self.titles_style = style.into();
        self
    }

    /// Sets the default [`Alignment`] for all block titles.
    ///
    /// Titles that explicitly set an [`Alignment`] will ignore this.
    ///
    /// # Example
    ///
    /// This example aligns all titles in the center except the "right" title which explicitly sets
    /// [`Alignment::Right`].
    /// ```
    /// use ratatui::layout::Alignment;
    /// use ratatui::text::Line;
    /// use ratatui::widgets::Block;
    ///
    /// Block::bordered()
    ///     .title_alignment(Alignment::Center)
    ///     // This title won't be aligned in the center
    ///     .title(Line::from("right").right_aligned())
    ///     .title("foo")
    ///     .title("bar");
    /// ```
    #[must_use = "method moves the value of self and returns the modified value"]
    pub const fn title_alignment(mut self, alignment: Alignment) -> Self {
        self.titles_alignment = alignment;
        self
    }

    /// Sets the default [`TitlePosition`] for all block titles.
    ///
    /// # Example
    ///
    /// This example positions all titles on the bottom by default. The "top" title explicitly sets
    /// its position to `Top`, so it is not affected. The "foo" and "bar" titles will be positioned
    /// at the bottom.
    ///
    /// ```
    /// use ratatui::widgets::{Block, TitlePosition};
    ///
    /// Block::bordered()
    ///     .title_position(TitlePosition::Bottom)
    ///     .title("foo") // will be at the bottom
    ///     .title_top("top") // will be at the top
    ///     .title("bar"); // will be at the bottom
    /// ```
    #[must_use = "method moves the value of self and returns the modified value"]
    pub const fn title_position(mut self, position: TitlePosition) -> Self {
        self.titles_position = position;
        self
    }

    /// Defines the style of the borders.
    ///
    /// This style is applied only to the areas covered by borders, and is applied to the block
    /// after any [`Block::style`] is applied.
    ///
    /// See [`Style`] for more information on how merging styles works.
    ///
    /// `style` accepts any type that is convertible to [`Style`] (e.g. [`Style`], [`Color`], or
    /// your own type that implements [`Into<Style>`]).
    ///
    /// # Example
    ///
    /// This example shows a `Block` with blue borders.
    /// ```
    /// use ratatui::style::{Style, Stylize};
    /// use ratatui::widgets::Block;
    /// Block::bordered().border_style(Style::new().blue());
    /// ```
    ///
    /// [`Color`]: ratatui_core::style::Color
    #[must_use = "method moves the value of self and returns the modified value"]
    pub fn border_style<S: Into<Style>>(mut self, style: S) -> Self {
        self.border_style = style.into();
        self
    }

    /// Defines the style of the entire block.
    ///
    /// This is the most generic [`Style`] a block can receive, it will be merged with any other
    /// more specific styles. Elements can be styled further with [`Block::title_style`] and
    /// [`Block::border_style`], which will be applied on top of this style. If the block is used as
    /// a container for another widget (e.g. a [`Paragraph`]), then the style of the widget is
    /// generally applied before this style.
    ///
    /// See [`Style`] for more information on how merging styles works.
    ///
    /// `style` accepts any type that is convertible to [`Style`] (e.g. [`Style`], [`Color`], or
    /// your own type that implements [`Into<Style>`]).
    ///
    /// # Example
    ///
    /// ```
    /// use ratatui::style::{Color, Style, Stylize};
    /// use ratatui::widgets::{Block, Paragraph};
    ///
    /// let block = Block::new().style(Style::new().red().on_black());
    ///
    /// // For border and title you can additionally apply styles on top of the block level style.
    /// let block = Block::new()
    ///     .style(Style::new().red().bold().italic())
    ///     .border_style(Style::new().not_italic()) // will be red and bold
    ///     .title_style(Style::new().not_bold()) // will be red and italic
    ///     .title("Title");
    ///
    /// // To style the inner widget, you can style the widget itself.
    /// let paragraph = Paragraph::new("Content")
    ///     .block(block)
    ///     .style(Style::new().white().not_bold()); // will be white, and italic
    /// ```
    ///
    /// [`Paragraph`]: crate::paragraph::Paragraph
    /// [`Color`]: ratatui_core::style::Color
    #[must_use = "method moves the value of self and returns the modified value"]
    pub fn style<S: Into<Style>>(mut self, style: S) -> Self {
        self.style = style.into();
        self
    }

    /// Defines which borders to display.
    ///
    /// [`Borders`] can also be styled with [`Block::border_style`] and [`Block::border_type`].
    ///
    /// # Examples
    ///
    /// Display left and right borders.
    /// ```
    /// use ratatui::widgets::{Block, Borders};
    /// Block::new().borders(Borders::LEFT | Borders::RIGHT);
    /// ```
    ///
    /// To show all borders you can abbreviate this with [`Block::bordered`]
    #[must_use = "method moves the value of self and returns the modified value"]
    pub const fn borders(mut self, flag: Borders) -> Self {
        self.borders = flag;
        self
    }

    /// Sets the symbols used to display the border (e.g. single line, double line, thick or
    /// rounded borders).
    ///
    /// Setting this overwrites any custom [`border_set`](Block::border_set) that was set.
    ///
    /// See [`BorderType`] for the full list of available symbols.
    ///
    /// # Examples
    ///
    /// ```
    /// use ratatui::widgets::{Block, BorderType};
    /// Block::bordered()
    ///     .border_type(BorderType::Rounded)
    ///     .title("Block");
    /// // Renders
    /// // ╭Block╮
    /// // │     │
    /// // ╰─────╯
    /// ```
    #[must_use = "method moves the value of self and returns the modified value"]
    pub const fn border_type(mut self, border_type: BorderType) -> Self {
        self.border_set = border_type.to_border_set();
        self
    }

    /// Sets the symbols used to display the border as a [`ratatui_core::symbols::border::Set`].
    ///
    /// Setting this overwrites any [`border_type`](Block::border_type) that was set.
    ///
    /// # Examples
    ///
    /// ```
    /// use ratatui::{widgets::Block, symbols};
    ///
    /// Block::bordered().border_set(symbols::border::DOUBLE).title("Block");
    /// // Renders
    /// // ╔Block╗
    /// // ║     ║
    /// // ╚═════╝
    #[must_use = "method moves the value of self and returns the modified value"]
    pub const fn border_set(mut self, border_set: border::Set<'a>) -> Self {
        self.border_set = border_set;
        self
    }

    /// Defines the padding inside a `Block`.
    ///
    /// See [`Padding`] for more information.
    ///
    /// # Examples
    ///
    /// This renders a `Block` with no padding (the default).
    /// ```
    /// use ratatui::widgets::{Block, Padding};
    ///
    /// Block::bordered().padding(Padding::ZERO);
    /// // Renders
    /// // ┌───────┐
    /// // │content│
    /// // └───────┘
    /// ```
    ///
    /// This example shows a `Block` with padding left and right ([`Padding::horizontal`]).
    /// Notice the two spaces before and after the content.
    /// ```
    /// use ratatui::widgets::{Block, Padding};
    ///
    /// Block::bordered().padding(Padding::horizontal(2));
    /// // Renders
    /// // ┌───────────┐
    /// // │  content  │
    /// // └───────────┘
    /// ```
    #[must_use = "method moves the value of self and returns the modified value"]
    pub const fn padding(mut self, padding: Padding) -> Self {
        self.padding = padding;
        self
    }

    /// Sets the block's [`MergeStrategy`] for overlapping characters, defaulting to `MergeStrategy::Replace`.
    ///
    /// Changing the strategy to `MergeStrategy::Exact` or `MergeStrategy::Fuzzy` collapses border characters that intersect
    /// with any previously rendered borders.
    ///
    /// For more information and examples, see the [collapse borders recipe] and [`MergeStrategy`]
    /// docs.
    ///
    /// # Example
    ///
    /// ```
    /// use ratatui::symbols::merge::MergeStrategy;
    /// # use ratatui::widgets::{Block, BorderType};
    ///
    /// // Given several blocks with plain borders (1)
    /// Block::bordered();
    /// // and other blocks with thick borders (2) which are rendered on top of the first
    /// Block::bordered()
    ///     .border_type(BorderType::Thick)
    ///     .merge_borders(MergeStrategy::Exact);
    /// ```
    ///
    /// Rendering these blocks with `MergeStrategy::Exact` or `MergeStrategy::Fuzzy` will collapse
    /// the borders, resulting in a clean layout without connected borders.
    ///
    /// ```plain
    /// ┌───┐    ┌───┐  ┌───┲━━━┓┌───┐
    /// │   │    │ 1 │  │   ┃   ┃│   │
    /// │ 1 │    │ ┏━┿━┓│ 1 ┃ 2 ┃│ 1 │
    /// │   │    │ ┃ │ ┃│   ┃   ┃│   │
    /// └───╆━━━┓└─╂─┘ ┃└───┺━━━┛┢━━━┪
    ///     ┃   ┃  ┃ 2 ┃         ┃   ┃
    ///     ┃ 2 ┃  ┗━━━┛         ┃ 2 ┃
    ///     ┃   ┃                ┃   ┃
    ///     ┗━━━┛                ┗━━━┛
    /// ```
    ///
    /// [collapse borders recipe]: https://ratatui.rs/recipes/layout/collapse-borders/
    /// `MergeStrategy::Replace`: MergeStrategy::Replace
    /// `MergeStrategy::Exact`: MergeStrategy::Exact
    /// `MergeStrategy::Fuzzy`: MergeStrategy::Fuzzy
    #[must_use = "method moves the value of self and returns the modified value"]
    pub const fn merge_borders(mut self, strategy: MergeStrategy) -> Self {
        self.merge_borders = strategy;
        self
    }

    /// Computes the inner area of a block after subtracting space for borders, titles, and padding.
    ///
    /// # Examples
    ///
    /// Draw a block nested within another block
    /// ```
    /// use ratatui::Frame;
    /// use ratatui::widgets::Block;
    ///
    /// # fn render_nested_block(frame: &mut Frame) {
    /// let outer_block = Block::bordered().title("Outer");
    /// let inner_block = Block::bordered().title("Inner");
    ///
    /// let outer_area = frame.area();
    /// let inner_area = outer_block.inner(outer_area);
    ///
    /// frame.render_widget(outer_block, outer_area);
    /// frame.render_widget(inner_block, inner_area);
    /// # }
    /// // Renders
    /// // ┌Outer────────┐
    /// // │┌Inner──────┐│
    /// // ││           ││
    /// // │└───────────┘│
    /// // └─────────────┘
    /// ```
    pub fn inner(&self, area: Rect) -> Rect {
        let mut inner = area;
        if self.borders.intersects(Borders::LEFT) {
            inner.x = inner.x.saturating_add(1).min(inner.right());
            inner.width = inner.width.saturating_sub(1);
        }
        if self.borders.intersects(Borders::TOP) || self.has_title_at_position(TitlePosition::Top) {
            inner.y = inner.y.saturating_add(1).min(inner.bottom());
            inner.height = inner.height.saturating_sub(1);
        }
        if self.borders.intersects(Borders::RIGHT) {
            inner.width = inner.width.saturating_sub(1);
        }
        if self.borders.intersects(Borders::BOTTOM)
            || self.has_title_at_position(TitlePosition::Bottom)
        {
            inner.height = inner.height.saturating_sub(1);
        }

        inner.x = inner.x.saturating_add(self.padding.left);
        inner.y = inner.y.saturating_add(self.padding.top);

        inner.width = inner
            .width
            .saturating_sub(self.padding.left + self.padding.right);
        inner.height = inner
            .height
            .saturating_sub(self.padding.top + self.padding.bottom);

        inner
    }

    fn has_title_at_position(&self, position: TitlePosition) -> bool {
        self.titles
            .iter()
            .any(|(pos, _)| pos.unwrap_or(self.titles_position) == position)
    }
}

impl Widget for Block<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Widget::render(&self, area, buf);
    }
}

impl Widget for &Block<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        buf.set_style(area, self.style);
        self.render_borders(area, buf);
        self.render_titles(area, buf);
    }
}

impl Block<'_> {
    fn render_borders(&self, area: Rect, buf: &mut Buffer) {
        self.render_sides(area, buf);
        self.render_corners(area, buf);
    }

    fn render_sides(&self, area: Rect, buf: &mut Buffer) {
        let left = area.left();
        let top = area.top();
        // area.right() and area.bottom() are outside the rect, subtract 1 to get the last row/col
        let right = area.right() - 1;
        let bottom = area.bottom() - 1;

        // The first and last element of each line are not drawn when there is an adjacent line as
        // this would cause the corner to initially be merged with a side character and then a
        // corner character to be drawn on top of it. Some merge strategies would not produce a
        // correct character in that case.
        let is_replace = self.merge_borders != MergeStrategy::Replace;
        let left_inset = left + u16::from(is_replace && self.borders.contains(Borders::LEFT));
        let top_inset = top + u16::from(is_replace && self.borders.contains(Borders::TOP));
        let right_inset = right - u16::from(is_replace && self.borders.contains(Borders::RIGHT));
        let bottom_inset = bottom - u16::from(is_replace && self.borders.contains(Borders::BOTTOM));

        let sides = [
            (
                Borders::LEFT,
                left..=left,
                top_inset..=bottom_inset,
                self.border_set.vertical_left,
            ),
            (
                Borders::TOP,
                left_inset..=right_inset,
                top..=top,
                self.border_set.horizontal_top,
            ),
            (
                Borders::RIGHT,
                right..=right,
                top_inset..=bottom_inset,
                self.border_set.vertical_right,
            ),
            (
                Borders::BOTTOM,
                left_inset..=right_inset,
                bottom..=bottom,
                self.border_set.horizontal_bottom,
            ),
        ];
        for (border, x_range, y_range, symbol) in sides {
            if self.borders.contains(border) {
                for x in x_range {
                    for y in y_range.clone() {
                        buf[(x, y)]
                            .merge_symbol(symbol, self.merge_borders)
                            .set_style(self.border_style);
                    }
                }
            }
        }
    }

    fn render_corners(&self, area: Rect, buf: &mut Buffer) {
        let corners = [
            (
                Borders::RIGHT | Borders::BOTTOM,
                area.right() - 1,
                area.bottom() - 1,
                self.border_set.bottom_right,
            ),
            (
                Borders::RIGHT | Borders::TOP,
                area.right() - 1,
                area.top(),
                self.border_set.top_right,
            ),
            (
                Borders::LEFT | Borders::BOTTOM,
                area.left(),
                area.bottom() - 1,
                self.border_set.bottom_left,
            ),
            (
                Borders::LEFT | Borders::TOP,
                area.left(),
                area.top(),
                self.border_set.top_left,
            ),
        ];

        for (border, x, y, symbol) in corners {
            if self.borders.contains(border) {
                buf[(x, y)]
                    .merge_symbol(symbol, self.merge_borders)
                    .set_style(self.border_style);
            }
        }
    }
    fn render_titles(&self, area: Rect, buf: &mut Buffer) {
        self.render_title_position(TitlePosition::Top, area, buf);
        self.render_title_position(TitlePosition::Bottom, area, buf);
    }

    fn render_title_position(&self, position: TitlePosition, area: Rect, buf: &mut Buffer) {
        // NOTE: the order in which these functions are called defines the overlapping behavior
        self.render_left_titles(position, area, buf);
        self.render_center_titles(position, area, buf);
        self.render_right_titles(position, area, buf);
    }

    /// Render titles aligned to the right of the block
    ///
    /// Currently (due to the way lines are truncated), the right side of the leftmost title will
    /// be cut off if the block is too small to fit all titles. This is not ideal and should be
    /// the left side of that leftmost that is cut off. This is due to the line being truncated
    /// incorrectly. See <https://github.com/ratatui/ratatui/issues/932>
    #[expect(clippy::similar_names)]
    fn render_right_titles(&self, position: TitlePosition, area: Rect, buf: &mut Buffer) {
        let titles = self.filtered_titles(position, Alignment::Right);
        let mut titles_area = self.titles_area(area, position);

        // render titles in reverse order to align them to the right
        for title in titles.rev() {
            if titles_area.is_empty() {
                break;
            }
            let title_width = title.width() as u16;
            let title_area = Rect {
                x: titles_area
                    .right()
                    .saturating_sub(title_width)
                    .max(titles_area.left()),
                width: title_width.min(titles_area.width),
                ..titles_area
            };
            buf.set_style(title_area, self.titles_style);
            title.render(title_area, buf);

            // bump the width of the titles area to the left
            titles_area.width = titles_area
                .width
                .saturating_sub(title_width)
                .saturating_sub(1); // space between titles
        }
    }

    /// Render titles in the center of the block
    fn render_center_titles(&self, position: TitlePosition, area: Rect, buf: &mut Buffer) {
        let area = self.titles_area(area, position);
        let titles = self
            .filtered_titles(position, Alignment::Center)
            .collect::<Vec<_>>();
        // titles are rendered with a space after each title except the last one
        let total_width = titles
            .iter()
            .map(|title| title.width() as u16 + 1)
            .sum::<u16>()
            .saturating_sub(1);

        if total_width <= area.width {
            self.render_centered_titles_without_truncation(titles, total_width, area, buf);
        } else {
            self.render_centered_titles_with_truncation(titles, total_width, area, buf);
        }
    }

    fn render_centered_titles_without_truncation(
        &self,
        titles: Vec<&Line<'_>>,
        total_width: u16,
        area: Rect,
        buf: &mut Buffer,
    ) {
        // titles fit in the area, center them
        let x = area.left() + area.width.saturating_sub(total_width) / 2;
        let mut area = Rect { x, ..area };
        for title in titles {
            let width = title.width() as u16;
            let title_area = Rect { width, ..area };
            buf.set_style(title_area, self.titles_style);
            title.render(title_area, buf);
            // Move the rendering cursor to the right, leaving 1 column space.
            area.x = area.x.saturating_add(width + 1);
            area.width = area.width.saturating_sub(width + 1);
        }
    }

    fn render_centered_titles_with_truncation(
        &self,
        titles: Vec<&Line<'_>>,
        total_width: u16,
        mut area: Rect,
        buf: &mut Buffer,
    ) {
        // titles do not fit in the area, truncate the left side using an offset. The right side
        // is truncated by the area width.
        let mut offset = total_width.saturating_sub(area.width) / 2;
        for title in titles {
            if area.is_empty() {
                break;
            }
            let width = area.width.min(title.width() as u16).saturating_sub(offset);
            let title_area = Rect { width, ..area };
            buf.set_style(title_area, self.titles_style);
            if offset > 0 {
                // truncate the left side of the title to fit the area
                title.clone().right_aligned().render(title_area, buf);
                offset = offset.saturating_sub(width).saturating_sub(1);
            } else {
                // truncate the right side of the title to fit the area if needed
                title.clone().left_aligned().render(title_area, buf);
            }
            // Leave 1 column of spacing between titles.
            area.x = area.x.saturating_add(width + 1);
            area.width = area.width.saturating_sub(width + 1);
        }
    }

    /// Render titles aligned to the left of the block
    #[expect(clippy::similar_names)]
    fn render_left_titles(&self, position: TitlePosition, area: Rect, buf: &mut Buffer) {
        let titles = self.filtered_titles(position, Alignment::Left);
        let mut titles_area = self.titles_area(area, position);
        for title in titles {
            if titles_area.is_empty() {
                break;
            }
            let title_width = title.width() as u16;
            let title_area = Rect {
                width: title_width.min(titles_area.width),
                ..titles_area
            };
            buf.set_style(title_area, self.titles_style);
            title.render(title_area, buf);

            // bump the titles area to the right and reduce its width
            titles_area.x = titles_area.x.saturating_add(title_width + 1);
            titles_area.width = titles_area.width.saturating_sub(title_width + 1);
        }
    }

    /// An iterator over the titles that match the position and alignment
    fn filtered_titles(
        &self,
        position: TitlePosition,
        alignment: Alignment,
    ) -> impl DoubleEndedIterator<Item = &Line<'_>> {
        self.titles
            .iter()
            .filter(move |(pos, _)| pos.unwrap_or(self.titles_position) == position)
            .filter(move |(_, line)| line.alignment.unwrap_or(self.titles_alignment) == alignment)
            .map(|(_, line)| line)
    }

    /// An area that is one line tall and spans the width of the block excluding the borders and
    /// is positioned at the top or bottom of the block.
    fn titles_area(&self, area: Rect, position: TitlePosition) -> Rect {
        let left_border = u16::from(self.borders.contains(Borders::LEFT));
        let right_border = u16::from(self.borders.contains(Borders::RIGHT));
        Rect {
            x: area.left() + left_border,
            y: match position {
                TitlePosition::Top => area.top(),
                TitlePosition::Bottom => area.bottom() - 1,
            },
            width: area
                .width
                .saturating_sub(left_border)
                .saturating_sub(right_border),
            height: 1,
        }
    }

    /// Calculate the left, and right space the [`Block`] will take up.
    ///
    /// The result takes the [`Block`]'s, [`Borders`], and [`Padding`] into account.
    pub(crate) fn horizontal_space(&self) -> (u16, u16) {
        let left = self
            .padding
            .left
            .saturating_add(u16::from(self.borders.contains(Borders::LEFT)));
        let right = self
            .padding
            .right
            .saturating_add(u16::from(self.borders.contains(Borders::RIGHT)));
        (left, right)
    }

    /// Calculate the top, and bottom space that the [`Block`] will take up.
    ///
    /// Takes the [`Padding`], [`TitlePosition`], and the [`Borders`] that are selected into
    /// account when calculating the result.
    pub(crate) fn vertical_space(&self) -> (u16, u16) {
        let has_top =
            self.borders.contains(Borders::TOP) || self.has_title_at_position(TitlePosition::Top);
        let top = self.padding.top + u16::from(has_top);
        let has_bottom = self.borders.contains(Borders::BOTTOM)
            || self.has_title_at_position(TitlePosition::Bottom);
        let bottom = self.padding.bottom + u16::from(has_bottom);
        (top, bottom)
    }
}

/// An extension trait for [`Block`] that provides some convenience methods.
///
/// This is implemented for [`Option<Block>`](Option) to simplify the common case of having a
/// widget with an optional block.
pub trait BlockExt {
    /// Return the inner area of the block if it is `Some`. Otherwise, returns `area`.
    ///
    /// This is a useful convenience method for widgets that have an `Option<Block>` field
    fn inner_if_some(&self, area: Rect) -> Rect;
}

impl BlockExt for Option<Block<'_>> {
    fn inner_if_some(&self, area: Rect) -> Rect {
        self.as_ref().map_or(area, |block| block.inner(area))
    }
}

impl Styled for Block<'_> {
    type Item = Self;

    fn style(&self) -> Style {
        self.style
    }

    fn set_style<S: Into<Style>>(self, style: S) -> Self::Item {
        self.style(style)
    }
}
