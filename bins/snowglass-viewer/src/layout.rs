use std::cell::RefCell;

use cssparser::{
    AtRuleParser, CowRcStr, DeclarationParser, ParseError, Parser, ParserInput, ParserState,
    QualifiedRuleParser, RuleBodyItemParser, RuleBodyParser,
};
use snowglass_input::mouse::layout::{Extent, Rect, aspect_fit};
use taffy::prelude::*;

thread_local! {
    static ACTIVE_LAYOUT: RefCell<Option<RenderTargetLayout>> = const { RefCell::new(None) };
}

pub(crate) const DEFAULT_CSS: &str = r#"#viewer {
    display: flex;
    flex-direction: column;
}

.render-target {
    flex-grow: 1;
    flex-shrink: 1;
    flex-basis: 0px;
    min-width: 0px;
    min-height: 0px;
}"#;

pub(crate) fn configure(css: Option<&str>) -> Result<(), String> {
    let layout = css.map(RenderTargetLayout::parse).transpose()?;
    ACTIVE_LAYOUT.with(|active| *active.borrow_mut() = layout);
    Ok(())
}

pub(crate) fn active_rectangles(
    extent: Extent,
    images: &[Extent],
    reverse: bool,
) -> Option<Vec<Rect>> {
    let mut rectangles = ACTIVE_LAYOUT.with(|active| {
        active
            .borrow()
            .as_ref()
            .and_then(|layout| layout.rectangles(extent, images))
    })?;
    if reverse {
        rectangles.reverse();
    }
    Some(rectangles)
}

#[derive(Clone, Debug)]
struct Rule {
    selectors: Vec<String>,
    style: Style,
    properties: Vec<String>,
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
                .map(|(name, value)| format!("{name}:{value}"))
                .collect::<Vec<_>>()
                .join(";");
            rules.push(Rule {
                selectors,
                style: parse_taffy_style(&normalized)?,
                properties: declarations.iter().map(|(name, _)| name.clone()).collect(),
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
        for (_, _, rule) in matching {
            merge_style(target, &rule.style, &rule.properties);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displayed_default_css_matches_the_builtin_layout() {
        let css = RenderTargetLayout::parse(DEFAULT_CSS).unwrap();
        assert_eq!(
            css.rectangles(
                Extent::new(100, 200),
                &[Extent::new(100, 100), Extent::new(100, 100)],
            )
            .unwrap(),
            [Rect::new(0, 0, 100, 100), Rect::new(0, 100, 100, 100)]
        );
    }

    #[test]
    fn ids_style_individual_render_targets() {
        let css = RenderTargetLayout::parse(
            "#viewer { display:flex; flex-direction:row } #render-target-0 { flex-grow:2 } .render-target { flex-grow:1 }",
        )
        .unwrap();
        let rects = css
            .rectangles(
                Extent::new(300, 100),
                &[Extent::new(200, 100), Extent::new(100, 100)],
            )
            .unwrap();
        assert_eq!(
            rects,
            [Rect::new(0, 0, 200, 100), Rect::new(200, 0, 100, 100)]
        );
    }

    #[test]
    fn grid_layout_uses_ids() {
        let css = RenderTargetLayout::parse(
            "#viewer { display:grid; grid-template-columns:1fr 1fr } #render-target-0 { grid-column:2 }",
        )
        .unwrap();
        assert!(
            css.rectangles(
                Extent::new(200, 100),
                &[Extent::new(100, 100), Extent::new(100, 100)],
            )
            .is_some()
        );
    }

    #[test]
    fn invalid_live_edit_keeps_the_last_valid_layout() {
        configure(Some(
            "#viewer { flex-direction:row } .render-target { flex-grow:1 }",
        ))
        .unwrap();
        let extent = Extent::new(200, 100);
        let images = [Extent::new(100, 100), Extent::new(100, 100)];
        let before = active_rectangles(extent, &images, false).unwrap();

        assert!(configure(Some("#viewer { flex-direction:")).is_err());
        assert_eq!(active_rectangles(extent, &images, false).unwrap(), before);
        configure(None).unwrap();
    }
}
