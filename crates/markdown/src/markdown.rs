pub mod parser;

use std::collections::{HashMap, HashSet};
use std::iter;
use std::mem;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    actions, point, quad, AnyElement, App, BorderStyle, Bounds, ClipboardItem, CursorStyle,
    DispatchPhase, Edges, Entity, FocusHandle, Focusable, FontStyle, FontWeight, GlobalElementId,
    Hitbox, Hsla, KeyContext, Length, MouseDownEvent, MouseEvent, MouseMoveEvent, MouseUpEvent,
    Point, Render, Stateful, StrikethroughStyle, StyleRefinement, StyledText, Task, TextLayout,
    TextRun, TextStyle, TextStyleRefinement,
};
use language::{Language, LanguageRegistry, Rope};
use parser::{parse_links_only, parse_markdown, MarkdownEvent, MarkdownTag, MarkdownTagEnd};
use pulldown_cmark::Alignment;
use theme::SyntaxTheme;
use ui::{prelude::*, Tooltip};
use util::{ResultExt, TryFutureExt};

use crate::parser::CodeBlockKind;

#[derive(Clone)]
pub struct MarkdownStyle {
    pub base_text_style: TextStyle,
    pub code_block: StyleRefinement,
    pub code_block_overflow_x_scroll: bool,
    pub inline_code: TextStyleRefinement,
    pub block_quote: TextStyleRefinement,
    pub link: TextStyleRefinement,
    pub rule_color: Hsla,
    pub block_quote_border_color: Hsla,
    pub syntax: Arc<SyntaxTheme>,
    pub selection_background_color: Hsla,
    pub heading: StyleRefinement,
    pub table_overflow_x_scroll: bool,
}

impl Default for MarkdownStyle {
    fn default() -> Self {
        Self {
            base_text_style: Default::default(),
            code_block: Default::default(),
            code_block_overflow_x_scroll: false,
            inline_code: Default::default(),
            block_quote: Default::default(),
            link: Default::default(),
            rule_color: Default::default(),
            block_quote_border_color: Default::default(),
            syntax: Arc::new(SyntaxTheme::default()),
            selection_background_color: Default::default(),
            heading: Default::default(),
            table_overflow_x_scroll: false,
        }
    }
}

pub struct Markdown {
    source: SharedString,
    selection: Selection,
    pressed_link: Option<RenderedLink>,
    autoscroll_request: Option<usize>,
    style: MarkdownStyle,
    parsed_markdown: ParsedMarkdown,
    should_reparse: bool,
    pending_parse: Option<Task<Option<()>>>,
    focus_handle: FocusHandle,
    language_registry: Option<Arc<LanguageRegistry>>,
    fallback_code_block_language: Option<String>,
    open_url: Option<Box<dyn Fn(SharedString, &mut Window, &mut App)>>,
    options: Options,
    copied_code_blocks: HashSet<ElementId>,
}

#[derive(Debug)]
struct Options {
    parse_links_only: bool,
    copy_code_block_buttons: bool,
}

actions!(markdown, [Copy]);

impl Markdown {
    pub fn new(
        source: SharedString,
        style: MarkdownStyle,
        language_registry: Option<Arc<LanguageRegistry>>,
        fallback_code_block_language: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let mut this = Self {
            source,
            selection: Selection::default(),
            pressed_link: None,
            autoscroll_request: None,
            style,
            should_reparse: false,
            parsed_markdown: ParsedMarkdown::default(),
            pending_parse: None,
            focus_handle,
            language_registry,
            fallback_code_block_language,
            options: Options {
                parse_links_only: false,
                copy_code_block_buttons: true,
            },
            open_url: None,
            copied_code_blocks: HashSet::new(),
        };
        this.parse(cx);
        this
    }

    pub fn open_url(
        self,
        open_url: impl Fn(SharedString, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            open_url: Some(Box::new(open_url)),
            ..self
        }
    }

    pub fn new_text(source: SharedString, style: MarkdownStyle, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let mut this = Self {
            source,
            selection: Selection::default(),
            pressed_link: None,
            autoscroll_request: None,
            style,
            should_reparse: false,
            parsed_markdown: ParsedMarkdown::default(),
            pending_parse: None,
            focus_handle,
            language_registry: None,
            fallback_code_block_language: None,
            options: Options {
                parse_links_only: true,
                copy_code_block_buttons: true,
            },
            open_url: None,
            copied_code_blocks: HashSet::new(),
        };
        this.parse(cx);
        this
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn append(&mut self, text: &str, cx: &mut Context<Self>) {
        self.source = SharedString::new(self.source.to_string() + text);
        self.parse(cx);
    }

    pub fn reset(&mut self, source: SharedString, cx: &mut Context<Self>) {
        if source == self.source() {
            return;
        }
        self.source = source;
        self.selection = Selection::default();
        self.autoscroll_request = None;
        self.pending_parse = None;
        self.should_reparse = false;
        self.parsed_markdown = ParsedMarkdown::default();
        self.parse(cx);
    }

    pub fn parsed_markdown(&self) -> &ParsedMarkdown {
        &self.parsed_markdown
    }

    fn copy(&self, text: &RenderedText, _: &mut Window, cx: &mut Context<Self>) {
        if self.selection.end <= self.selection.start {
            return;
        }
        let text = text.text_for_range(self.selection.start..self.selection.end);
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    fn parse(&mut self, cx: &mut Context<Self>) {
        if self.source.is_empty() {
            return;
        }

        if self.pending_parse.is_some() {
            self.should_reparse = true;
            return;
        }

        let source = self.source.clone();
        let parse_text_only = self.options.parse_links_only;
        let language_registry = self.language_registry.clone();
        let fallback = self.fallback_code_block_language.clone();
        let parsed = cx.background_spawn(async move {
            if parse_text_only {
                return anyhow::Ok(ParsedMarkdown {
                    events: Arc::from(parse_links_only(source.as_ref())),
                    source,
                    languages: HashMap::default(),
                });
            }
            let (events, language_names) = parse_markdown(&source);
            let mut languages = HashMap::with_capacity(language_names.len());
            for name in language_names {
                if let Some(registry) = language_registry.as_ref() {
                    let language = if !name.is_empty() {
                        registry.language_for_name(&name)
                    } else if let Some(fallback) = &fallback {
                        registry.language_for_name(fallback)
                    } else {
                        continue;
                    };
                    if let Ok(language) = language.await {
                        languages.insert(name, language);
                    }
                }
            }
            anyhow::Ok(ParsedMarkdown {
                source,
                events: Arc::from(events),
                languages,
            })
        });

        self.should_reparse = false;
        self.pending_parse = Some(cx.spawn(async move |this, cx| {
            async move {
                let parsed = parsed.await?;
                this.update(cx, |this, cx| {
                    this.parsed_markdown = parsed;
                    this.pending_parse.take();
                    if this.should_reparse {
                        this.parse(cx);
                    }
                    cx.notify();
                })
                .ok();
                anyhow::Ok(())
            }
            .log_err()
            .await
        }));
    }

    pub fn copy_code_block_buttons(mut self, should_copy: bool) -> Self {
        self.options.copy_code_block_buttons = should_copy;
        self
    }
}

impl Render for Markdown {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        MarkdownElement::new(cx.entity().clone(), self.style.clone())
    }
}

impl Focusable for Markdown {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

#[derive(Copy, Clone, Default, Debug)]
struct Selection {
    start: usize,
    end: usize,
    reversed: bool,
    pending: bool,
}

impl Selection {
    fn set_head(&mut self, head: usize) {
        if head < self.tail() {
            if !self.reversed {
                self.end = self.start;
                self.reversed = true;
            }
            self.start = head;
        } else {
            if self.reversed {
                self.start = self.end;
                self.reversed = false;
            }
            self.end = head;
        }
    }

    fn tail(&self) -> usize {
        if self.reversed {
            self.end
        } else {
            self.start
        }
    }
}

#[derive(Default)]
pub struct ParsedMarkdown {
    source: SharedString,
    events: Arc<[(Range<usize>, MarkdownEvent)]>,
    languages: HashMap<SharedString, Arc<Language>>,
}

impl ParsedMarkdown {
    pub fn source(&self) -> &SharedString {
        &self.source
    }

    pub fn events(&self) -> &Arc<[(Range<usize>, MarkdownEvent)]> {
        &self.events
    }
}

pub struct MarkdownElement {
    markdown: Entity<Markdown>,
    style: MarkdownStyle,
}

impl MarkdownElement {
    fn new(markdown: Entity<Markdown>, style: MarkdownStyle) -> Self {
        Self { markdown, style }
    }

    fn paint_selection(
        &self,
        bounds: Bounds<Pixels>,
        rendered_text: &RenderedText,
        window: &mut Window,
        cx: &mut App,
    ) {
        let selection = self.markdown.read(cx).selection;
        let selection_start = rendered_text.position_for_source_index(selection.start);
        let selection_end = rendered_text.position_for_source_index(selection.end);

        if let Some(((start_position, start_line_height), (end_position, end_line_height))) =
            selection_start.zip(selection_end)
        {
            if start_position.y == end_position.y {
                window.paint_quad(quad(
                    Bounds::from_corners(
                        start_position,
                        point(end_position.x, end_position.y + end_line_height),
                    ),
                    Pixels::ZERO,
                    self.style.selection_background_color,
                    Edges::default(),
                    Hsla::transparent_black(),
                    BorderStyle::default(),
                ));
            } else {
                window.paint_quad(quad(
                    Bounds::from_corners(
                        start_position,
                        point(bounds.right(), start_position.y + start_line_height),
                    ),
                    Pixels::ZERO,
                    self.style.selection_background_color,
                    Edges::default(),
                    Hsla::transparent_black(),
                    BorderStyle::default(),
                ));

                if end_position.y > start_position.y + start_line_height {
                    window.paint_quad(quad(
                        Bounds::from_corners(
                            point(bounds.left(), start_position.y + start_line_height),
                            point(bounds.right(), end_position.y),
                        ),
                        Pixels::ZERO,
                        self.style.selection_background_color,
                        Edges::default(),
                        Hsla::transparent_black(),
                        BorderStyle::default(),
                    ));
                }

                window.paint_quad(quad(
                    Bounds::from_corners(
                        point(bounds.left(), end_position.y),
                        point(end_position.x, end_position.y + end_line_height),
                    ),
                    Pixels::ZERO,
                    self.style.selection_background_color,
                    Edges::default(),
                    Hsla::transparent_black(),
                    BorderStyle::default(),
                ));
            }
        }
    }

    fn paint_mouse_listeners(
        &self,
        hitbox: &Hitbox,
        rendered_text: &RenderedText,
        window: &mut Window,
        cx: &mut App,
    ) {
        let is_hovering_link = hitbox.is_hovered(window)
            && !self.markdown.read(cx).selection.pending
            && rendered_text
                .link_for_position(window.mouse_position())
                .is_some();

        if is_hovering_link {
            window.set_cursor_style(CursorStyle::PointingHand, Some(hitbox));
        } else {
            window.set_cursor_style(CursorStyle::IBeam, Some(hitbox));
        }

        self.on_mouse_event(window, cx, {
            let rendered_text = rendered_text.clone();
            let hitbox = hitbox.clone();
            move |markdown, event: &MouseDownEvent, phase, window, cx| {
                if hitbox.is_hovered(window) {
                    if phase.bubble() {
                        match rendered_text.link_for_position(event.position) { Some(link) => {
                            markdown.pressed_link = Some(link.clone());
                        } _ => {
                            let source_index =
                                match rendered_text.source_index_for_position(event.position) {
                                    Ok(ix) | Err(ix) => ix,
                                };
                            let range = if event.click_count == 2 {
                                rendered_text.surrounding_word_range(source_index)
                            } else if event.click_count == 3 {
                                rendered_text.surrounding_line_range(source_index)
                            } else {
                                source_index..source_index
                            };
                            markdown.selection = Selection {
                                start: range.start,
                                end: range.end,
                                reversed: false,
                                pending: true,
                            };
                            window.focus(&markdown.focus_handle);
                            window.prevent_default();
                        }}

                        cx.notify();
                    }
                } else if phase.capture() {
                    markdown.selection = Selection::default();
                    markdown.pressed_link = None;
                    cx.notify();
                }
            }
        });
        self.on_mouse_event(window, cx, {
            let rendered_text = rendered_text.clone();
            let hitbox = hitbox.clone();
            let was_hovering_link = is_hovering_link;
            move |markdown, event: &MouseMoveEvent, phase, window, cx| {
                if phase.capture() {
                    return;
                }

                if markdown.selection.pending {
                    let source_index = match rendered_text.source_index_for_position(event.position)
                    {
                        Ok(ix) | Err(ix) => ix,
                    };
                    markdown.selection.set_head(source_index);
                    markdown.autoscroll_request = Some(source_index);
                    cx.notify();
                } else {
                    let is_hovering_link = hitbox.is_hovered(window)
                        && rendered_text.link_for_position(event.position).is_some();
                    if is_hovering_link != was_hovering_link {
                        cx.notify();
                    }
                }
            }
        });
        self.on_mouse_event(window, cx, {
            let rendered_text = rendered_text.clone();
            move |markdown, event: &MouseUpEvent, phase, window, cx| {
                if phase.bubble() {
                    if let Some(pressed_link) = markdown.pressed_link.take() {
                        if Some(&pressed_link) == rendered_text.link_for_position(event.position) {
                            match markdown.open_url.as_mut() { Some(open_url) => {
                                open_url(pressed_link.destination_url, window, cx);
                            } _ => {
                                cx.open_url(&pressed_link.destination_url);
                            }}
                        }
                    }
                } else if markdown.selection.pending {
                    markdown.selection.pending = false;
                    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
                    {
                        let text = rendered_text
                            .text_for_range(markdown.selection.start..markdown.selection.end);
                        cx.write_to_primary(ClipboardItem::new_string(text))
                    }
                    cx.notify();
                }
            }
        });
    }

    fn autoscroll(
        &self,
        rendered_text: &RenderedText,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<()> {
        let autoscroll_index = self
            .markdown
            .update(cx, |markdown, _| markdown.autoscroll_request.take())?;
        let (position, line_height) = rendered_text.position_for_source_index(autoscroll_index)?;

        let text_style = self.style.base_text_style.clone();
        let font_id = window.text_system().resolve_font(&text_style.font());
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let em_width = window.text_system().em_width(font_id, font_size).unwrap();
        window.request_autoscroll(Bounds::from_corners(
            point(position.x - 3. * em_width, position.y - 3. * line_height),
            point(position.x + 3. * em_width, position.y + 3. * line_height),
        ));
        Some(())
    }

    fn on_mouse_event<T: MouseEvent>(
        &self,
        window: &mut Window,
        _cx: &mut App,
        mut f: impl 'static
            + FnMut(&mut Markdown, &T, DispatchPhase, &mut Window, &mut Context<Markdown>),
    ) {
        window.on_mouse_event({
            let markdown = self.markdown.downgrade();
            move |event, phase, window, cx| {
                markdown
                    .update(cx, |markdown, cx| f(markdown, event, phase, window, cx))
                    .log_err();
            }
        });
    }
}

impl Element for MarkdownElement {
    type RequestLayoutState = RenderedMarkdown;
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (gpui::LayoutId, Self::RequestLayoutState) {
        let mut builder = MarkdownElementBuilder::new(
            self.style.base_text_style.clone(),
            self.style.syntax.clone(),
        );
        let parsed_markdown = &self.markdown.read(cx).parsed_markdown;
        let markdown_end = if let Some(last) = parsed_markdown.events.last() {
            last.0.end
        } else {
            0
        };
        for (range, event) in parsed_markdown.events.iter() {
            match event {
                MarkdownEvent::Start(tag) => {
                    match tag {
                        MarkdownTag::Paragraph => {
                            builder.push_div(
                                div().mb_2().line_height(rems(1.3)),
                                range,
                                markdown_end,
                            );
                        }
                        MarkdownTag::Heading { level, .. } => {
                            let mut heading = div().mb_2();
                            heading = match level {
                                pulldown_cmark::HeadingLevel::H1 => heading.text_3xl(),
                                pulldown_cmark::HeadingLevel::H2 => heading.text_2xl(),
                                pulldown_cmark::HeadingLevel::H3 => heading.text_xl(),
                                pulldown_cmark::HeadingLevel::H4 => heading.text_lg(),
                                _ => heading,
                            };
                            heading.style().refine(&self.style.heading);
                            builder.push_text_style(
                                self.style.heading.text_style().clone().unwrap_or_default(),
                            );
                            builder.push_div(heading, range, markdown_end);
                        }
                        MarkdownTag::BlockQuote => {
                            builder.push_text_style(self.style.block_quote.clone());
                            builder.push_div(
                                div()
                                    .pl_4()
                                    .mb_2()
                                    .border_l_4()
                                    .border_color(self.style.block_quote_border_color),
                                range,
                                markdown_end,
                            );
                        }
                        MarkdownTag::CodeBlock(kind) => {
                            let language = if let CodeBlockKind::Fenced(language) = kind {
                                parsed_markdown.languages.get(language).cloned()
                            } else {
                                None
                            };

                            // This is a parent container that we can position the copy button inside.
                            builder.push_div(div().relative().w_full(), range, markdown_end);

                            let mut code_block = div()
                                .id(("code-block", range.start))
                                .rounded_lg()
                                .map(|mut code_block| {
                                    if self.style.code_block_overflow_x_scroll {
                                        code_block.style().restrict_scroll_to_axis = Some(true);
                                        code_block.flex().overflow_x_scroll()
                                    } else {
                                        code_block.w_full()
                                    }
                                });
                            code_block.style().refine(&self.style.code_block);
                            if let Some(code_block_text_style) = &self.style.code_block.text {
                                builder.push_text_style(code_block_text_style.to_owned());
                            }
                            builder.push_code_block(language);
                            builder.push_div(code_block, range, markdown_end);
                        }
                        MarkdownTag::HtmlBlock => builder.push_div(div(), range, markdown_end),
                        MarkdownTag::List(bullet_index) => {
                            builder.push_list(*bullet_index);
                            builder.push_div(div().pl_4(), range, markdown_end);
                        }
                        MarkdownTag::Item => {
                            let bullet = match builder.next_bullet_index() { Some(bullet_index) => {
                                format!("{}.", bullet_index)
                            } _ => {
                                "•".to_string()
                            }};
                            builder.push_div(
                                div()
                                    .mb_1()
                                    .h_flex()
                                    .items_start()
                                    .gap_1()
                                    .line_height(rems(1.3))
                                    .child(bullet),
                                range,
                                markdown_end,
                            );
                            // Without `w_0`, text doesn't wrap to the width of the container.
                            builder.push_div(div().flex_1().w_0(), range, markdown_end);
                        }
                        MarkdownTag::Emphasis => builder.push_text_style(TextStyleRefinement {
                            font_style: Some(FontStyle::Italic),
                            ..Default::default()
                        }),
                        MarkdownTag::Strong => builder.push_text_style(TextStyleRefinement {
                            font_weight: Some(FontWeight::BOLD),
                            ..Default::default()
                        }),
                        MarkdownTag::Strikethrough => {
                            builder.push_text_style(TextStyleRefinement {
                                strikethrough: Some(StrikethroughStyle {
                                    thickness: px(1.),
                                    color: None,
                                }),
                                ..Default::default()
                            })
                        }
                        MarkdownTag::Link { dest_url, .. } => {
                            if builder.code_block_stack.is_empty() {
                                builder.push_link(dest_url.clone(), range.clone());
                                builder.push_text_style(self.style.link.clone())
                            }
                        }
                        MarkdownTag::MetadataBlock(_) => {}
                        MarkdownTag::Table(alignments) => {
                            builder.table_alignments = alignments.clone();
                            builder.push_div(
                                div()
                                    .id(("table", range.start))
                                    .flex()
                                    .border_1()
                                    .border_color(cx.theme().colors().border)
                                    .rounded_sm()
                                    .when(self.style.table_overflow_x_scroll, |mut table| {
                                        table.style().restrict_scroll_to_axis = Some(true);
                                        table.overflow_x_scroll()
                                    }),
                                range,
                                markdown_end,
                            );
                            // This inner `v_flex` is so the table rows will stack vertically without disrupting the `overflow_x_scroll`.
                            builder.push_div(div().v_flex().flex_grow(), range, markdown_end);
                        }
                        MarkdownTag::TableHead => {
                            builder.push_div(
                                div()
                                    .flex()
                                    .justify_between()
                                    .border_b_1()
                                    .border_color(cx.theme().colors().border),
                                range,
                                markdown_end,
                            );
                            builder.push_text_style(TextStyleRefinement {
                                font_weight: Some(FontWeight::BOLD),
                                ..Default::default()
                            });
                        }
                        MarkdownTag::TableRow => {
                            builder.push_div(
                                div().h_flex().justify_between().px_1().py_0p5(),
                                range,
                                markdown_end,
                            );
                        }
                        MarkdownTag::TableCell => {
                            let column_count = builder.table_alignments.len();

                            builder.push_div(
                                div()
                                    .flex()
                                    .px_1()
                                    .w(relative(1. / column_count as f32))
                                    .truncate(),
                                range,
                                markdown_end,
                            );
                        }
                        _ => log::debug!("unsupported markdown tag {:?}", tag),
                    }
                }
                MarkdownEvent::End(tag) => match tag {
                    MarkdownTagEnd::Paragraph => {
                        builder.pop_div();
                    }
                    MarkdownTagEnd::Heading(_) => {
                        builder.pop_div();
                        builder.pop_text_style()
                    }
                    MarkdownTagEnd::BlockQuote(_kind) => {
                        builder.pop_text_style();
                        builder.pop_div()
                    }
                    MarkdownTagEnd::CodeBlock => {
                        builder.trim_trailing_newline();

                        builder.pop_div();
                        builder.pop_code_block();
                        if self.style.code_block.text.is_some() {
                            builder.pop_text_style();
                        }

                        if self.markdown.read(cx).options.copy_code_block_buttons {
                            builder.flush_text();
                            builder.modify_current_div(|el| {
                                let id =
                                    ElementId::NamedInteger("copy-markdown-code".into(), range.end);
                                let was_copied =
                                    self.markdown.read(cx).copied_code_blocks.contains(&id);
                                let copy_button = div().absolute().top_1().right_1().w_5().child(
                                    IconButton::new(
                                        id.clone(),
                                        if was_copied {
                                            IconName::Check
                                        } else {
                                            IconName::Copy
                                        },
                                    )
                                    .icon_color(Color::Muted)
                                    .shape(ui::IconButtonShape::Square)
                                    .tooltip(Tooltip::text("Copy Code"))
                                    .on_click({
                                        let id = id.clone();
                                        let markdown = self.markdown.clone();
                                        let code = without_fences(
                                            parsed_markdown.source()[range.clone()].trim(),
                                        )
                                        .to_string();
                                        move |_event, _window, cx| {
                                            let id = id.clone();
                                            markdown.update(cx, |this, cx| {
                                                this.copied_code_blocks.insert(id.clone());

                                                cx.write_to_clipboard(ClipboardItem::new_string(
                                                    code.clone(),
                                                ));

                                                cx.spawn(async move |this, cx| {
                                                    cx.background_executor()
                                                        .timer(Duration::from_secs(2))
                                                        .await;

                                                    cx.update(|cx| {
                                                        this.update(cx, |this, cx| {
                                                            this.copied_code_blocks.remove(&id);
                                                            cx.notify();
                                                        })
                                                    })
                                                    .ok();
                                                })
                                                .detach();
                                            });
                                        }
                                    }),
                                );

                                el.child(copy_button)
                            });
                        }

                        // Pop the parent container.
                        builder.pop_div();
                    }
                    MarkdownTagEnd::HtmlBlock => builder.pop_div(),
                    MarkdownTagEnd::List(_) => {
                        builder.pop_list();
                        builder.pop_div();
                    }
                    MarkdownTagEnd::Item => {
                        builder.pop_div();
                        builder.pop_div();
                    }
                    MarkdownTagEnd::Emphasis => builder.pop_text_style(),
                    MarkdownTagEnd::Strong => builder.pop_text_style(),
                    MarkdownTagEnd::Strikethrough => builder.pop_text_style(),
                    MarkdownTagEnd::Link => {
                        if builder.code_block_stack.is_empty() {
                            builder.pop_text_style()
                        }
                    }
                    MarkdownTagEnd::Table => {
                        builder.pop_div();
                        builder.pop_div();
                        builder.table_alignments.clear();
                    }
                    MarkdownTagEnd::TableHead => {
                        builder.pop_div();
                        builder.pop_text_style();
                    }
                    MarkdownTagEnd::TableRow => {
                        builder.pop_div();
                    }
                    MarkdownTagEnd::TableCell => {
                        builder.pop_div();
                    }
                    _ => log::debug!("unsupported markdown tag end: {:?}", tag),
                },
                MarkdownEvent::Text(parsed) => {
                    builder.push_text(parsed, range.start);
                }
                MarkdownEvent::Code => {
                    builder.push_text_style(self.style.inline_code.clone());
                    builder.push_text(&parsed_markdown.source[range.clone()], range.start);
                    builder.pop_text_style();
                }
                MarkdownEvent::Html => {
                    builder.push_text(&parsed_markdown.source[range.clone()], range.start);
                }
                MarkdownEvent::InlineHtml => {
                    builder.push_text(&parsed_markdown.source[range.clone()], range.start);
                }
                MarkdownEvent::Rule => {
                    builder.push_div(
                        div()
                            .border_b_1()
                            .my_2()
                            .border_color(self.style.rule_color),
                        range,
                        markdown_end,
                    );
                    builder.pop_div()
                }
                MarkdownEvent::SoftBreak => builder.push_text(" ", range.start),
                MarkdownEvent::HardBreak => builder.push_text("\n", range.start),
                _ => log::error!("unsupported markdown event {:?}", event),
            }
        }
        let mut rendered_markdown = builder.build();
        let child_layout_id = rendered_markdown.element.request_layout(window, cx);
        let layout_id = window.request_layout(gpui::Style::default(), [child_layout_id], cx);
        (layout_id, rendered_markdown)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        bounds: Bounds<Pixels>,
        rendered_markdown: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let focus_handle = self.markdown.read(cx).focus_handle.clone();
        window.set_focus_handle(&focus_handle, cx);

        let hitbox = window.insert_hitbox(bounds, false);
        rendered_markdown.element.prepaint(window, cx);
        self.autoscroll(&rendered_markdown.text, window, cx);
        hitbox
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        bounds: Bounds<Pixels>,
        rendered_markdown: &mut Self::RequestLayoutState,
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let mut context = KeyContext::default();
        context.add("Markdown");
        window.set_key_context(context);
        let entity = self.markdown.clone();
        window.on_action(std::any::TypeId::of::<crate::Copy>(), {
            let text = rendered_markdown.text.clone();
            move |_, phase, window, cx| {
                let text = text.clone();
                if phase == DispatchPhase::Bubble {
                    entity.update(cx, move |this, cx| this.copy(&text, window, cx))
                }
            }
        });

        self.paint_mouse_listeners(hitbox, &rendered_markdown.text, window, cx);
        rendered_markdown.element.paint(window, cx);
        self.paint_selection(bounds, &rendered_markdown.text, window, cx);
    }
}

impl IntoElement for MarkdownElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

enum AnyDiv {
    Div(Div),
    Stateful(Stateful<Div>),
}

impl AnyDiv {
    fn into_any_element(self) -> AnyElement {
        match self {
            Self::Div(div) => div.into_any_element(),
            Self::Stateful(div) => div.into_any_element(),
        }
    }
}

impl From<Div> for AnyDiv {
    fn from(value: Div) -> Self {
        Self::Div(value)
    }
}

impl From<Stateful<Div>> for AnyDiv {
    fn from(value: Stateful<Div>) -> Self {
        Self::Stateful(value)
    }
}

impl Styled for AnyDiv {
    fn style(&mut self) -> &mut StyleRefinement {
        match self {
            Self::Div(div) => div.style(),
            Self::Stateful(div) => div.style(),
        }
    }
}

impl ParentElement for AnyDiv {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        match self {
            Self::Div(div) => div.extend(elements),
            Self::Stateful(div) => div.extend(elements),
        }
    }
}

struct MarkdownElementBuilder {
    div_stack: Vec<AnyDiv>,
    rendered_lines: Vec<RenderedLine>,
    pending_line: PendingLine,
    rendered_links: Vec<RenderedLink>,
    current_source_index: usize,
    base_text_style: TextStyle,
    text_style_stack: Vec<TextStyleRefinement>,
    code_block_stack: Vec<Option<Arc<Language>>>,
    list_stack: Vec<ListStackEntry>,
    table_alignments: Vec<Alignment>,
    syntax_theme: Arc<SyntaxTheme>,
}

#[derive(Default)]
struct PendingLine {
    text: String,
    runs: Vec<TextRun>,
    source_mappings: Vec<SourceMapping>,
}

struct ListStackEntry {
    bullet_index: Option<u64>,
}

impl MarkdownElementBuilder {
    fn new(base_text_style: TextStyle, syntax_theme: Arc<SyntaxTheme>) -> Self {
        Self {
            div_stack: vec![div().debug_selector(|| "inner".into()).into()],
            rendered_lines: Vec::new(),
            pending_line: PendingLine::default(),
            rendered_links: Vec::new(),
            current_source_index: 0,
            base_text_style,
            text_style_stack: Vec::new(),
            code_block_stack: Vec::new(),
            list_stack: Vec::new(),
            table_alignments: Vec::new(),
            syntax_theme,
        }
    }

    fn push_text_style(&mut self, style: TextStyleRefinement) {
        self.text_style_stack.push(style);
    }

    fn text_style(&self) -> TextStyle {
        let mut style = self.base_text_style.clone();
        for refinement in &self.text_style_stack {
            style.refine(refinement);
        }
        style
    }

    fn pop_text_style(&mut self) {
        self.text_style_stack.pop();
    }

    fn push_div(&mut self, div: impl Into<AnyDiv>, range: &Range<usize>, markdown_end: usize) {
        let mut div = div.into();
        self.flush_text();

        if range.start == 0 {
            // Remove the top margin on the first element.
            div.style().refine(&StyleRefinement {
                margin: gpui::EdgesRefinement {
                    top: Some(Length::Definite(px(0.).into())),
                    left: None,
                    right: None,
                    bottom: None,
                },
                ..Default::default()
            });
        }

        if range.end == markdown_end {
            div.style().refine(&StyleRefinement {
                margin: gpui::EdgesRefinement {
                    top: None,
                    left: None,
                    right: None,
                    bottom: Some(Length::Definite(rems(0.).into())),
                },
                ..Default::default()
            });
        }

        self.div_stack.push(div);
    }

    fn modify_current_div(&mut self, f: impl FnOnce(AnyDiv) -> AnyDiv) {
        self.flush_text();
        if let Some(div) = self.div_stack.pop() {
            self.div_stack.push(f(div));
        }
    }

    fn pop_div(&mut self) {
        self.flush_text();
        let div = self.div_stack.pop().unwrap().into_any_element();
        self.div_stack.last_mut().unwrap().extend(iter::once(div));
    }

    fn push_list(&mut self, bullet_index: Option<u64>) {
        self.list_stack.push(ListStackEntry { bullet_index });
    }

    fn next_bullet_index(&mut self) -> Option<u64> {
        self.list_stack.last_mut().and_then(|entry| {
            let item_index = entry.bullet_index.as_mut()?;
            *item_index += 1;
            Some(*item_index - 1)
        })
    }

    fn pop_list(&mut self) {
        self.list_stack.pop();
    }

    fn push_code_block(&mut self, language: Option<Arc<Language>>) {
        self.code_block_stack.push(language);
    }

    fn pop_code_block(&mut self) {
        self.code_block_stack.pop();
    }

    fn push_link(&mut self, destination_url: SharedString, source_range: Range<usize>) {
        self.rendered_links.push(RenderedLink {
            source_range,
            destination_url,
        });
    }

    fn push_text(&mut self, text: &str, source_index: usize) {
        self.pending_line.source_mappings.push(SourceMapping {
            rendered_index: self.pending_line.text.len(),
            source_index,
        });
        self.pending_line.text.push_str(text);
        self.current_source_index = source_index + text.len();

        match self.code_block_stack.last() { Some(Some(language)) => {
            let mut offset = 0;
            for (range, highlight_id) in language.highlight_text(&Rope::from(text), 0..text.len()) {
                if range.start > offset {
                    self.pending_line
                        .runs
                        .push(self.text_style().to_run(range.start - offset));
                }

                let mut run_style = self.text_style();
                if let Some(highlight) = highlight_id.style(&self.syntax_theme) {
                    run_style = run_style.highlight(highlight);
                }
                self.pending_line.runs.push(run_style.to_run(range.len()));
                offset = range.end;
            }

            if offset < text.len() {
                self.pending_line
                    .runs
                    .push(self.text_style().to_run(text.len() - offset));
            }
        } _ => {
            self.pending_line
                .runs
                .push(self.text_style().to_run(text.len()));
        }}
    }

    fn trim_trailing_newline(&mut self) {
        if self.pending_line.text.ends_with('\n') {
            self.pending_line
                .text
                .truncate(self.pending_line.text.len() - 1);
            self.pending_line.runs.last_mut().unwrap().len -= 1;
            self.current_source_index -= 1;
        }
    }

    fn flush_text(&mut self) {
        let line = mem::take(&mut self.pending_line);
        if line.text.is_empty() {
            return;
        }

        let text = StyledText::new(line.text).with_runs(line.runs);
        self.rendered_lines.push(RenderedLine {
            layout: text.layout().clone(),
            source_mappings: line.source_mappings,
            source_end: self.current_source_index,
        });
        self.div_stack.last_mut().unwrap().extend([text.into_any()]);
    }

    fn build(mut self) -> RenderedMarkdown {
        debug_assert_eq!(self.div_stack.len(), 1);
        self.flush_text();
        RenderedMarkdown {
            element: self.div_stack.pop().unwrap().into_any_element(),
            text: RenderedText {
                lines: self.rendered_lines.into(),
                links: self.rendered_links.into(),
            },
        }
    }
}

struct RenderedLine {
    layout: TextLayout,
    source_mappings: Vec<SourceMapping>,
    source_end: usize,
}

impl RenderedLine {
    fn rendered_index_for_source_index(&self, source_index: usize) -> usize {
        let mapping = match self
            .source_mappings
            .binary_search_by_key(&source_index, |probe| probe.source_index)
        {
            Ok(ix) => &self.source_mappings[ix],
            Err(ix) => &self.source_mappings[ix - 1],
        };
        mapping.rendered_index + (source_index - mapping.source_index)
    }

    fn source_index_for_rendered_index(&self, rendered_index: usize) -> usize {
        let mapping = match self
            .source_mappings
            .binary_search_by_key(&rendered_index, |probe| probe.rendered_index)
        {
            Ok(ix) => &self.source_mappings[ix],
            Err(ix) => &self.source_mappings[ix - 1],
        };
        mapping.source_index + (rendered_index - mapping.rendered_index)
    }

    fn source_index_for_position(&self, position: Point<Pixels>) -> Result<usize, usize> {
        let line_rendered_index;
        let out_of_bounds;
        match self.layout.index_for_position(position) {
            Ok(ix) => {
                line_rendered_index = ix;
                out_of_bounds = false;
            }
            Err(ix) => {
                line_rendered_index = ix;
                out_of_bounds = true;
            }
        };
        let source_index = self.source_index_for_rendered_index(line_rendered_index);
        if out_of_bounds {
            Err(source_index)
        } else {
            Ok(source_index)
        }
    }
}

#[derive(Copy, Clone, Debug, Default)]
struct SourceMapping {
    rendered_index: usize,
    source_index: usize,
}

pub struct RenderedMarkdown {
    element: AnyElement,
    text: RenderedText,
}

#[derive(Clone)]
struct RenderedText {
    lines: Rc<[RenderedLine]>,
    links: Rc<[RenderedLink]>,
}

#[derive(Clone, Eq, PartialEq)]
struct RenderedLink {
    source_range: Range<usize>,
    destination_url: SharedString,
}

impl RenderedText {
    fn source_index_for_position(&self, position: Point<Pixels>) -> Result<usize, usize> {
        let mut lines = self.lines.iter().peekable();

        while let Some(line) = lines.next() {
            let line_bounds = line.layout.bounds();
            if position.y > line_bounds.bottom() {
                if let Some(next_line) = lines.peek() {
                    if position.y < next_line.layout.bounds().top() {
                        return Err(line.source_end);
                    }
                }

                continue;
            }

            return line.source_index_for_position(position);
        }

        Err(self.lines.last().map_or(0, |line| line.source_end))
    }

    fn position_for_source_index(&self, source_index: usize) -> Option<(Point<Pixels>, Pixels)> {
        for line in self.lines.iter() {
            let line_source_start = line.source_mappings.first().unwrap().source_index;
            if source_index < line_source_start {
                break;
            } else if source_index > line.source_end {
                continue;
            } else {
                let line_height = line.layout.line_height();
                let rendered_index_within_line = line.rendered_index_for_source_index(source_index);
                let position = line.layout.position_for_index(rendered_index_within_line)?;
                return Some((position, line_height));
            }
        }
        None
    }

    fn surrounding_word_range(&self, source_index: usize) -> Range<usize> {
        for line in self.lines.iter() {
            if source_index > line.source_end {
                continue;
            }

            let line_rendered_start = line.source_mappings.first().unwrap().rendered_index;
            let rendered_index_in_line =
                line.rendered_index_for_source_index(source_index) - line_rendered_start;
            let text = line.layout.text();
            let previous_space = if let Some(idx) = text[0..rendered_index_in_line].rfind(' ') {
                idx + ' '.len_utf8()
            } else {
                0
            };
            let next_space = if let Some(idx) = text[rendered_index_in_line..].find(' ') {
                rendered_index_in_line + idx
            } else {
                text.len()
            };

            return line.source_index_for_rendered_index(line_rendered_start + previous_space)
                ..line.source_index_for_rendered_index(line_rendered_start + next_space);
        }

        source_index..source_index
    }

    fn surrounding_line_range(&self, source_index: usize) -> Range<usize> {
        for line in self.lines.iter() {
            if source_index > line.source_end {
                continue;
            }
            let line_source_start = line.source_mappings.first().unwrap().source_index;
            return line_source_start..line.source_end;
        }

        source_index..source_index
    }

    fn text_for_range(&self, range: Range<usize>) -> String {
        let mut ret = vec![];

        for line in self.lines.iter() {
            if range.start > line.source_end {
                continue;
            }
            let line_source_start = line.source_mappings.first().unwrap().source_index;
            if range.end < line_source_start {
                break;
            }

            let text = line.layout.text();

            let start = if range.start < line_source_start {
                0
            } else {
                line.rendered_index_for_source_index(range.start)
            };
            let end = if range.end > line.source_end {
                line.rendered_index_for_source_index(line.source_end)
            } else {
                line.rendered_index_for_source_index(range.end)
            }
            .min(text.len());

            ret.push(text[start..end].to_string());
        }
        ret.join("\n")
    }

    fn link_for_position(&self, position: Point<Pixels>) -> Option<&RenderedLink> {
        let source_index = self.source_index_for_position(position).ok()?;
        self.links
            .iter()
            .find(|link| link.source_range.contains(&source_index))
    }
}

/// Some markdown blocks are indented, and others have e.g. ```rust … ``` around them.
/// If this block is fenced with backticks, strip them off (and the language name).
/// We use this when copying code blocks to the clipboard.
fn without_fences(mut markdown: &str) -> &str {
    if let Some(opening_backticks) = markdown.find("```") {
        markdown = &markdown[opening_backticks..];

        // Trim off the next newline. This also trims off a language name if it's there.
        if let Some(newline) = markdown.find('\n') {
            markdown = &markdown[newline + 1..];
        }
    };

    if let Some(closing_backticks) = markdown.rfind("```") {
        markdown = &markdown[..closing_backticks];
    };

    markdown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_without_fences() {
        let input = "```rust\nlet x = 5;\n```";
        assert_eq!(without_fences(input), "let x = 5;\n");

        let input = "   ```\nno language\n```   ";
        assert_eq!(without_fences(input), "no language\n");

        let input = "plain text";
        assert_eq!(without_fences(input), "plain text");

        let input = "```python\nprint('hello')\nprint('world')\n```";
        assert_eq!(without_fences(input), "print('hello')\nprint('world')\n");
    }
}
