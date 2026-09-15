use parking_lot::RwLock;
use std::cell::RefCell;

use cssparser::{
    AtRuleParser, CowRcStr, DeclarationParser, ParseError, Parser, ParserInput, ParserState,
    QualifiedRuleParser, RuleBodyItemParser, RuleBodyParser,
};
use cssparser_color::Color;
use polymer_renderer::layout::{Extent, Rect, aspect_fit};
use polymer_renderer::{CompositingPlan, Presentation, SurfacePlacement};
use taffy::prelude::*;

#[derive(Default)]
struct LayoutSource {
    revision: u64,
    css: Option<String>,
}

static LAYOUT_SOURCE: RwLock<LayoutSource> = RwLock::new(LayoutSource {
    revision: 0,
    css: None,
});

thread_local! {
    static ACTIVE_LAYOUT: RefCell<(u64, Option<RenderTargetLayout>)> =
        const { RefCell::new((0, None)) };
}

pub const DEFAULT_CSS: &str = r#"#viewer {
    display: flex;
    flex-direction: column;
    background-color: #ff00ff;
}

.render-target {
    flex-grow: 1;
    flex-shrink: 1;
    flex-basis: 0px;
    min-width: 0px;
    min-height: 0px;
}"#;

pub fn configure(css: Option<&str>) -> Result<(), String> {
    css.map(RenderTargetLayout::parse).transpose()?;
    let mut source = LAYOUT_SOURCE.write();
    source.revision = source.revision.wrapping_add(1);
    source.css = css.map(str::to_owned);
    Ok(())
}

pub fn active_rectangles(extent: Extent, images: &[Extent], reverse: bool) -> Option<Vec<Rect>> {
    let mut rectangles =
        with_active_layout(|layout| layout.and_then(|layout| layout.rectangles(extent, images)))?;
    if reverse {
        rectangles.reverse();
    }
    Some(rectangles)
}

pub fn clear_color() -> [f32; 4] {
    with_active_layout(|layout| {
        layout.map_or([1.0, 0.0, 1.0, 1.0], RenderTargetLayout::viewer_background)
    })
}

/// Everything a plan is derived from. Nothing else about a surface reaches the
/// layout, so two presentations with equal keys always produce equal plans.
#[derive(Clone, Eq, PartialEq)]
struct PlanKey {
    revision: u64,
    extent: Extent,
    surfaces: Vec<(u64, u64, Extent, bool)>,
}

thread_local! {
    static PLAN_CACHE: RefCell<Option<(PlanKey, CompositingPlan)>> = const { RefCell::new(None) };
}

/// Resolves a presentation into the plan the compositor draws.
///
/// Callers ask for the plan several times per frame — once to draw, and again
/// whenever a pointer moves or a guest window is measured — and each miss
/// builds a Taffy tree, matches selectors, and formats per-target selector
/// strings. Between two such calls the inputs are almost always identical, so
/// the last answer is kept and returned until one of them actually changes.
pub fn composition_plan(presentation: &Presentation) -> CompositingPlan {
    let surfaces = presentation.surfaces();
    let key = PlanKey {
        revision: layout_revision(),
        extent: presentation.viewer_extent(),
        surfaces: surfaces
            .iter()
            .map(|surface| {
                (
                    surface.connection,
                    surface.surface_id,
                    surface.image_extent,
                    surface.flip_y,
                )
            })
            .collect(),
    };
    if let Some(plan) = PLAN_CACHE.with(|cache| {
        cache
            .borrow()
            .as_ref()
            .filter(|(cached, _)| *cached == key)
            .map(|(_, plan)| plan.clone())
    }) {
        return plan;
    }

    let images = surfaces
        .iter()
        .map(|surface| surface.image_extent)
        .collect::<Vec<_>>();
    let reverse = surfaces.len() > 1 && surfaces.iter().all(|surface| surface.flip_y);
    let rectangles = active_rectangles(presentation.viewer_extent(), &images, reverse)
        .unwrap_or_else(|| default_rectangles(presentation.viewer_extent(), &images, reverse));
    let placements = surfaces
        .iter()
        .zip(rectangles)
        .map(|(surface, rectangle)| {
            SurfacePlacement::new(surface.connection, surface.surface_id, rectangle, 0.0)
        })
        .collect();
    let plan = CompositingPlan::new(clear_color(), placements);
    PLAN_CACHE.with(|cache| *cache.borrow_mut() = Some((key, plan.clone())));
    plan
}

fn layout_revision() -> u64 {
    LAYOUT_SOURCE.read().revision
}

fn default_rectangles(extent: Extent, images: &[Extent], reverse: bool) -> Vec<Rect> {
    let count = u32::try_from(images.len()).unwrap_or(u32::MAX).max(1);
    let cell_height = extent.height / count;
    let mut rectangles = images
        .iter()
        .enumerate()
        .map(|(index, image)| {
            let row = u32::try_from(index).unwrap_or(u32::MAX);
            let bounds = Rect::new(
                0,
                i32::try_from(row.saturating_mul(cell_height)).unwrap_or(i32::MAX),
                extent.width,
                cell_height,
            );
            aspect_fit(*image, bounds).unwrap_or(bounds)
        })
        .collect::<Vec<_>>();
    if reverse {
        rectangles.reverse();
    }
    rectangles
}

fn with_active_layout<T>(read: impl FnOnce(Option<&RenderTargetLayout>) -> T) -> T {
    let source = LAYOUT_SOURCE.read();
    ACTIVE_LAYOUT.with(|active| {
        let mut active = active.borrow_mut();
        if active.0 != source.revision {
            active.1 = source
                .css
                .as_deref()
                .map(RenderTargetLayout::parse)
                .transpose()
                .expect("render-target CSS was validated before publication");
            active.0 = source.revision;
        }
        read(active.1.as_ref())
    })
}

#[derive(Clone, Debug)]
struct Rule {
    selectors: Vec<String>,
    style: Style,
    properties: Vec<String>,
    background_color: Option<[f32; 4]>,
}

/// A cssparser stylesheet whose supported layout declarations are consumed by
/// Taffy for the tiny viewer-only tree.
#[derive(Clone, Debug)]
pub(crate) struct RenderTargetLayout {
    rules: Vec<Rule>,
}

impl RenderTargetLayout {
    pub(crate) fn parse(css: &str) -> Result<Self, String> {
        let mut rest = css;
        let mut rules = Vec::new();
        while !rest.trim().is_empty() {
            rest = rest.trim_start();
            let Some(open) = rest.find('{') else {
                return Err("render-target CSS rule is missing `{`".to_owned());
            };
            let selectors = rest[..open]
                .split(',')
                .map(str::trim)
                .filter(|selector| !selector.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>();
            if selectors.is_empty() {
                return Err("render-target CSS rule has no selector".to_owned());
            }
            let body_start = open + 1;
            let Some(close) = rest[body_start..].find('}') else {
                return Err("render-target CSS rule is missing `}`".to_owned());
            };
            let body_end = body_start + close;
            let body = &rest[body_start..body_end];
            let declarations = parse_declarations_with_cssparser(body)?;
            let normalized = declarations
                .iter()
                .filter(|(name, _)| name != "background-color")
                .map(|(name, value)| format!("{name}:{value}"))
                .collect::<Vec<_>>()
                .join(";");
            rules.push(Rule {
                selectors,
                style: parse_taffy_style(&normalized)?,
                background_color: declarations
                    .iter()
                    .rev()
                    .find(|(name, _)| name == "background-color")
                    .map(|(_, value)| parse_color(value))
                    .transpose()?,
                properties: declarations
                    .iter()
                    .filter(|(name, _)| name != "background-color")
                    .map(|(name, _)| name.clone())
                    .collect(),
            });
            rest = &rest[body_end + 1..];
        }
        Ok(Self { rules })
    }

    fn rectangles(&self, extent: Extent, images: &[Extent]) -> Option<Vec<Rect>> {
        if extent.is_empty() {
            return None;
        }
        let mut tree: TaffyTree<()> = TaffyTree::new();
        let children = images
            .iter()
            .enumerate()
            .map(|(index, image)| {
                let mut style = Style {
                    flex_grow: 1.0,
                    flex_basis: length(0.0_f32),
                    min_size: Size {
                        width: length(0.0_f32),
                        height: length(0.0_f32),
                    },
                    ..Style::default()
                };
                self.apply(
                    &[
                        "render-target",
                        ".render-target",
                        &format!("#render-target-{index}"),
                    ],
                    &mut style,
                );
                tree.new_leaf(style).map(|node| (node, *image))
            })
            .collect::<Result<Vec<_>, _>>()
            .ok()?;

        let nodes = children.iter().map(|(node, _)| *node).collect::<Vec<_>>();
        let mut root_style = Style {
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            ..Style::default()
        };
        self.apply(&["viewer", "#viewer"], &mut root_style);
        root_style.size = Size {
            width: length(extent.width as f32),
            height: length(extent.height as f32),
        };
        let root = tree.new_with_children(root_style, &nodes).ok()?;
        tree.compute_layout(
            root,
            Size {
                width: AvailableSpace::Definite(extent.width as f32),
                height: AvailableSpace::Definite(extent.height as f32),
            },
        )
        .ok()?;

        children
            .into_iter()
            .map(|(node, image)| {
                let layout = tree.layout(node).ok()?;
                aspect_fit(
                    image,
                    Rect::new(
                        layout.location.x.round() as i32,
                        layout.location.y.round() as i32,
                        layout.size.width.max(0.0).round() as u32,
                        layout.size.height.max(0.0).round() as u32,
                    ),
                )
            })
            .collect()
    }

    fn apply(&self, selectors: &[&str], target: &mut Style) {
        for (_, _, rule) in self.matching_rules(selectors) {
            merge_style(target, &rule.style, &rule.properties);
        }
    }

    fn viewer_background(&self) -> [f32; 4] {
        let mut color = [1.0, 0.0, 1.0, 1.0];
        for (_, _, rule) in self.matching_rules(&["viewer", "#viewer"]) {
            if let Some(value) = rule.background_color {
                color = value;
            }
        }
        color
    }

    fn matching_rules<'a>(&'a self, selectors: &[&str]) -> Vec<(u8, usize, &'a Rule)> {
        let mut matching = self
            .rules
            .iter()
            .enumerate()
            .filter_map(|(source_order, rule)| {
                let specificity = rule
                    .selectors
                    .iter()
                    .filter(|selector| selectors.contains(&selector.as_str()))
                    .map(|selector| match selector.as_bytes().first() {
                        Some(b'#') => 100,
                        Some(b'.') => 10,
                        _ => 1,
                    })
                    .max()?;
                Some((specificity, source_order, rule))
            })
            .collect::<Vec<_>>();
        matching.sort_by_key(|(specificity, source_order, _)| (*specificity, *source_order));
        matching
    }
}

struct LayoutDeclarationParser;

impl<'i> DeclarationParser<'i> for LayoutDeclarationParser {
    type Declaration = (String, String);
    type Error = ();

    fn parse_value<'t>(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i, 't>,
        _declaration_start: &ParserState,
    ) -> Result<Self::Declaration, ParseError<'i, Self::Error>> {
        let start = input.position();
        while input.next_including_whitespace_and_comments().is_ok() {}
        let value = input.slice_from(start).trim();
        if value.is_empty() {
            return Err(input.new_custom_error(()));
        }
        Ok((name.to_ascii_lowercase(), value.to_owned()))
    }
}

impl<'i> AtRuleParser<'i> for LayoutDeclarationParser {
    type Prelude = ();
    type AtRule = (String, String);
    type Error = ();
}

impl<'i> QualifiedRuleParser<'i> for LayoutDeclarationParser {
    type Prelude = ();
    type QualifiedRule = (String, String);
    type Error = ();
}

impl<'i> RuleBodyItemParser<'i, (String, String), ()> for LayoutDeclarationParser {
    fn parse_declarations(&self) -> bool {
        true
    }

    fn parse_qualified(&self) -> bool {
        false
    }
}

fn parse_declarations_with_cssparser(css: &str) -> Result<Vec<(String, String)>, String> {
    let mut input = ParserInput::new(css);
    let mut input = Parser::new(&mut input);
    RuleBodyParser::new(&mut input, &mut LayoutDeclarationParser)
        .map(|result| {
            result.map_err(|(error, declaration)| {
                format!(
                    "invalid CSS declaration `{declaration}` at {:?}",
                    error.location
                )
            })
        })
        .collect()
}

fn parse_taffy_style(css: &str) -> Result<Style, String> {
    let mut style = Style::default();
    for declaration in css
        .split(';')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let (name, value) = declaration
            .split_once(':')
            .ok_or_else(|| format!("invalid CSS declaration `{declaration}`"))?;
        let (name, value) = (name.trim(), value.trim());
        match name {
            "display" => style.display = parse(name, value)?,
            "position" => style.position = parse(name, value)?,
            "flex-direction" => style.flex_direction = parse(name, value)?,
            "flex-wrap" => style.flex_wrap = parse(name, value)?,
            "flex-grow" => style.flex_grow = parse(name, value)?,
            "flex-shrink" => style.flex_shrink = parse(name, value)?,
            "flex-basis" => style.flex_basis = parse(name, value)?,
            "align-items" => style.align_items = Some(parse(name, value)?),
            "align-content" => style.align_content = Some(parse(name, value)?),
            "align-self" => style.align_self = Some(parse(name, value)?),
            "justify-content" => style.justify_content = Some(parse(name, value)?),
            "justify-self" => style.justify_self = Some(parse(name, value)?),
            "width" => style.size.width = parse(name, value)?,
            "height" => style.size.height = parse(name, value)?,
            "min-width" => style.min_size.width = parse(name, value)?,
            "min-height" => style.min_size.height = parse(name, value)?,
            "max-width" => style.max_size.width = parse(name, value)?,
            "max-height" => style.max_size.height = parse(name, value)?,
            "aspect-ratio" => style.aspect_ratio = Some(parse(name, value)?),
            "gap" => style.gap = parse_gap(value)?,
            "row-gap" => style.gap.height = parse(name, value)?,
            "column-gap" => style.gap.width = parse(name, value)?,
            "padding" => style.padding = parse_rect(value, name)?,
            "grid-template-columns" => {
                style.grid_template_columns = parse_grid_tracks(value, name)?
            }
            "grid-template-rows" => style.grid_template_rows = parse_grid_tracks(value, name)?,
            "grid-auto-flow" => style.grid_auto_flow = parse(name, value)?,
            "grid-column" => style.grid_column = parse_grid_line(value, name)?,
            "grid-row" => style.grid_row = parse_grid_line(value, name)?,
            other => return Err(format!("unsupported Taffy layout property `{other}`")),
        }
    }
    Ok(style)
}

fn parse_color(value: &str) -> Result<[f32; 4], String> {
    let mut input = ParserInput::new(value);
    let mut input = Parser::new(&mut input);
    let color = Color::parse(&mut input).map_err(|error| {
        format!(
            "invalid `background-color: {value}` at {:?}",
            error.location
        )
    })?;
    input.expect_exhausted().map_err(|error| {
        format!(
            "invalid `background-color: {value}` at {:?}",
            error.location
        )
    })?;
    let Color::Rgba(color) = color else {
        return Err(
            "background-color currently supports named, hexadecimal, rgb(), and rgba() colors"
                .to_owned(),
        );
    };
    Ok([
        f32::from(color.red) / 255.0,
        f32::from(color.green) / 255.0,
        f32::from(color.blue) / 255.0,
        color.alpha,
    ])
}

fn merge_style(target: &mut Style, source: &Style, properties: &[String]) {
    for property in properties {
        match property.as_str() {
            "display" => target.display = source.display,
            "position" => target.position = source.position,
            "width" => target.size.width = source.size.width,
            "height" => target.size.height = source.size.height,
            "min-width" => target.min_size.width = source.min_size.width,
            "min-height" => target.min_size.height = source.min_size.height,
            "max-width" => target.max_size.width = source.max_size.width,
            "max-height" => target.max_size.height = source.max_size.height,
            "aspect-ratio" => target.aspect_ratio = source.aspect_ratio,
            "flex-direction" => target.flex_direction = source.flex_direction,
            "flex-wrap" => target.flex_wrap = source.flex_wrap,
            "flex-basis" => target.flex_basis = source.flex_basis,
            "flex-grow" => target.flex_grow = source.flex_grow,
            "flex-shrink" => target.flex_shrink = source.flex_shrink,
            "align-items" => target.align_items = source.align_items,
            "align-content" => target.align_content = source.align_content,
            "align-self" => target.align_self = source.align_self,
            "justify-content" => target.justify_content = source.justify_content,
            "justify-self" => target.justify_self = source.justify_self,
            "gap" => target.gap = source.gap,
            "row-gap" => target.gap.height = source.gap.height,
            "column-gap" => target.gap.width = source.gap.width,
            "padding" => target.padding = source.padding,
            "grid-template-columns" => {
                target.grid_template_columns = source.grid_template_columns.clone()
            }
            "grid-template-rows" => target.grid_template_rows = source.grid_template_rows.clone(),
            "grid-auto-flow" => target.grid_auto_flow = source.grid_auto_flow,
            "grid-column" => target.grid_column = source.grid_column.clone(),
            "grid-row" => target.grid_row = source.grid_row.clone(),
            _ => {}
        }
    }
}

fn parse<T: std::str::FromStr>(name: &str, value: &str) -> Result<T, String>
where
    T::Err: std::fmt::Display,
{
    value
        .parse()
        .map_err(|error| format!("invalid `{name}: {value}`: {error}"))
}

fn parse_gap(value: &str) -> Result<Size<LengthPercentage>, String> {
    let values = value.split_whitespace().collect::<Vec<_>>();
    match values.as_slice() {
        [both] => {
            let both = parse("gap", both)?;
            Ok(Size {
                width: both,
                height: both,
            })
        }
        [row, column] => Ok(Size {
            width: parse("gap", column)?,
            height: parse("gap", row)?,
        }),
        _ => Err(format!("invalid `gap: {value}`")),
    }
}

fn parse_rect(value: &str, name: &str) -> Result<taffy::geometry::Rect<LengthPercentage>, String> {
    let values = value
        .split_whitespace()
        .map(|part| parse(name, part))
        .collect::<Result<Vec<_>, _>>()?;
    let (top, right, bottom, left) = match values.as_slice() {
        [all] => (*all, *all, *all, *all),
        [vertical, horizontal] => (*vertical, *horizontal, *vertical, *horizontal),
        [top, horizontal, bottom] => (*top, *horizontal, *bottom, *horizontal),
        [top, right, bottom, left] => (*top, *right, *bottom, *left),
        _ => return Err(format!("invalid `{name}: {value}`")),
    };
    Ok(taffy::geometry::Rect {
        left,
        right,
        top,
        bottom,
    })
}

fn parse_grid_tracks(
    value: &str,
    name: &str,
) -> Result<Vec<GridTemplateComponent<String>>, String> {
    let tracks: taffy::style::GridTemplateTracks<String, GridTemplateComponent<String>> =
        parse(name, value)?;
    Ok(tracks.tracks)
}

fn parse_grid_line(
    value: &str,
    name: &str,
) -> Result<taffy::geometry::Line<GridPlacement>, String> {
    let mut values = value.split('/').map(str::trim);
    let start = parse(name, values.next().unwrap_or("auto"))?;
    let end = values
        .next()
        .map(|value| parse(name, value))
        .transpose()?
        .unwrap_or_default();
    if values.next().is_some() {
        return Err(format!("invalid `{name}: {value}`"));
    }
    Ok(taffy::geometry::Line { start, end })
}
