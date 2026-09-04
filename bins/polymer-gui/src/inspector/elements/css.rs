//! CSS stylesheet discovery, parsing, and CDP style projection.

use super::protocol::DocumentProtocol;
use blitz_dom::{BaseDocument, Node, NodeId};
use serde_json::{Value, json};

#[derive(Clone)]
pub(super) struct InspectorStylesheet {
    pub(super) id: String,
    pub(super) node_id: NodeId,
    pub(super) title: String,
    pub(super) source_url: String,
    pub(super) text: String,
}

#[derive(Clone)]
struct ParsedRule {
    selector_text: String,
    selector_start: usize,
    selector_end: usize,
    selectors: Vec<(String, usize, usize)>,
    declarations: Vec<ParsedDeclaration>,
    body_start: usize,
    body_end: usize,
}

#[derive(Clone)]
struct ParsedDeclaration {
    name: String,
    value: String,
    important: bool,
    start: usize,
    end: usize,
}

pub(super) struct Stylesheets<'a> {
    document: &'a BaseDocument,
}

impl<'a> Stylesheets<'a> {
    pub(super) fn new(document: &'a BaseDocument) -> Self {
        Self { document }
    }

    pub(super) fn all(&self) -> Vec<InspectorStylesheet> {
        self.document
            .tree()
            .iter()
            .filter_map(|(node_id, node)| InspectorStylesheet::from_node(node_id, node))
            .collect()
    }
}

impl InspectorStylesheet {
    fn from_node(node_id: NodeId, node: &Node) -> Option<Self> {
        let element = node.element_data()?;
        if element.name.local.as_ref() != "style" {
            return None;
        }
        let source = element
            .attrs
            .iter()
            .find(|attribute| attribute.name.local.as_ref() == "data-vite-dev-id")
            .map(|attribute| attribute.value.clone())
            .unwrap_or_default();
        let title = if source.is_empty() {
            format!(
                "inline stylesheet {}",
                DocumentProtocol::protocol_id(node_id)
            )
        } else {
            source.replace('\\', "/")
        };
        Some(Self {
            id: format!("polymer-style-{}", DocumentProtocol::protocol_id(node_id)),
            node_id,
            title,
            source_url: Self::source_path_url(&source),
            text: node.text_content(),
        })
    }

    fn source_path_url(source: &str) -> String {
        let normalized = source.replace('\\', "/");
        if normalized.len() >= 3
            && normalized.as_bytes()[1] == b':'
            && normalized.as_bytes()[2] == b'/'
        {
            format!("file:///{normalized}")
        } else {
            normalized
        }
    }
}

struct CssParser;

impl CssParser {
    fn parse_rules(css: &str) -> Vec<ParsedRule> {
        let mut rules = Vec::new();
        Self::parse_rule_range(css, 0, css.len(), &mut rules);
        rules
    }

    fn parse_rule_range(css: &str, mut cursor: usize, end: usize, rules: &mut Vec<ParsedRule>) {
        while cursor < end {
            cursor = Self::skip_css_trivia(css, cursor, end);
            if cursor >= end {
                break;
            }
            let Some(open) = Self::find_css_token(css, cursor, end, b'{') else {
                break;
            };
            let Some(close) = Self::matching_brace(css, open, end) else {
                break;
            };
            let header_start = Self::trim_start(css, cursor, open);
            let header_end = Self::trim_end(css, header_start, open);
            let header = &css[header_start..header_end];
            if header.starts_with("@media")
                || header.starts_with("@supports")
                || header.starts_with("@layer")
                || header.starts_with("@container")
            {
                Self::parse_rule_range(css, open + 1, close, rules);
            } else if !header.starts_with('@') && !header.is_empty() {
                let selectors = Self::split_css_list(css, header_start, header_end);
                rules.push(ParsedRule {
                    selector_text: header.to_owned(),
                    selector_start: header_start,
                    selector_end: header_end,
                    selectors,
                    declarations: Self::parse_declarations(css, open + 1, close),
                    body_start: open + 1,
                    body_end: close,
                });
            }
            cursor = close + 1;
        }
    }

    fn skip_css_trivia(css: &str, mut cursor: usize, end: usize) -> usize {
        let bytes = css.as_bytes();
        loop {
            while cursor < end && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            if cursor + 1 < end && bytes[cursor] == b'/' && bytes[cursor + 1] == b'*' {
                cursor += 2;
                while cursor + 1 < end && !(bytes[cursor] == b'*' && bytes[cursor + 1] == b'/') {
                    cursor += 1;
                }
                cursor = (cursor + 2).min(end);
                continue;
            }
            return cursor;
        }
    }

    fn find_css_token(css: &str, start: usize, end: usize, token: u8) -> Option<usize> {
        let bytes = css.as_bytes();
        let mut quote = None;
        let mut escaped = false;
        let mut comment = false;
        for index in start..end {
            let byte = bytes[index];
            if comment {
                if index > start && bytes[index - 1] == b'*' && byte == b'/' {
                    comment = false;
                }
                continue;
            }
            if quote.is_none() && index + 1 < end && byte == b'/' && bytes[index + 1] == b'*' {
                comment = true;
                continue;
            }
            if let Some(active) = quote {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == active {
                    quote = None;
                }
                continue;
            }
            if byte == b'\'' || byte == b'"' {
                quote = Some(byte);
            } else if byte == token {
                return Some(index);
            }
        }
        None
    }

    fn matching_brace(css: &str, open: usize, end: usize) -> Option<usize> {
        let bytes = css.as_bytes();
        let mut depth = 0usize;
        let mut quote = None;
        let mut escaped = false;
        let mut comment = false;
        let mut index = open;
        while index < end {
            let byte = bytes[index];
            if comment {
                if index > open && bytes[index - 1] == b'*' && byte == b'/' {
                    comment = false;
                }
                index += 1;
                continue;
            }
            if quote.is_none() && index + 1 < end && byte == b'/' && bytes[index + 1] == b'*' {
                comment = true;
                index += 2;
                continue;
            }
            if let Some(active) = quote {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == active {
                    quote = None;
                }
            } else if byte == b'\'' || byte == b'"' {
                quote = Some(byte);
            } else if byte == b'{' {
                depth += 1;
            } else if byte == b'}' {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index);
                }
            }
            index += 1;
        }
        None
    }

    fn split_css_list(css: &str, start: usize, end: usize) -> Vec<(String, usize, usize)> {
        Self::split_css_segments(css, start, end, b',')
            .into_iter()
            .map(|(start, end)| (css[start..end].to_owned(), start, end))
            .collect()
    }

    fn split_css_segments(
        css: &str,
        start: usize,
        end: usize,
        separator: u8,
    ) -> Vec<(usize, usize)> {
        let bytes = css.as_bytes();
        let mut segments = Vec::new();
        let mut segment = start;
        let mut parentheses = 0usize;
        let mut brackets = 0usize;
        let mut quote = None;
        let mut escaped = false;
        for index in start..end {
            let byte = bytes[index];
            if let Some(active) = quote {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == active {
                    quote = None;
                }
                continue;
            }
            match byte {
                b'\'' | b'"' => quote = Some(byte),
                b'(' => parentheses += 1,
                b')' => parentheses = parentheses.saturating_sub(1),
                b'[' => brackets += 1,
                b']' => brackets = brackets.saturating_sub(1),
                byte if byte == separator && parentheses == 0 && brackets == 0 => {
                    let trimmed_start = Self::trim_start(css, segment, index);
                    let trimmed_end = Self::trim_end(css, trimmed_start, index);
                    if trimmed_start < trimmed_end {
                        segments.push((trimmed_start, trimmed_end));
                    }
                    segment = index + 1;
                }
                _ => {}
            }
        }
        let trimmed_start = Self::trim_start(css, segment, end);
        let trimmed_end = Self::trim_end(css, trimmed_start, end);
        if trimmed_start < trimmed_end {
            segments.push((trimmed_start, trimmed_end));
        }
        segments
    }

    fn parse_declarations(css: &str, start: usize, end: usize) -> Vec<ParsedDeclaration> {
        Self::split_css_segments(css, start, end, b';')
            .into_iter()
            .filter_map(|(start, end)| {
                let colon = Self::find_css_token(css, start, end, b':')?;
                let name_start = Self::trim_start(css, start, colon);
                let name_end = Self::trim_end(css, name_start, colon);
                let value_start = Self::trim_start(css, colon + 1, end);
                let value_end = Self::trim_end(css, value_start, end);
                if name_start == name_end || value_start == value_end {
                    return None;
                }
                let mut value = css[value_start..value_end].to_owned();
                let important = value
                    .to_ascii_lowercase()
                    .strip_suffix("!important")
                    .is_some();
                if important {
                    value.truncate(value.len() - "!important".len());
                    value = value.trim_end().to_owned();
                }
                Some(ParsedDeclaration {
                    name: css[name_start..name_end].to_owned(),
                    value,
                    important,
                    start,
                    end,
                })
            })
            .collect()
    }

    fn trim_start(css: &str, mut start: usize, end: usize) -> usize {
        while start < end && css.as_bytes()[start].is_ascii_whitespace() {
            start += 1;
        }
        start
    }

    fn trim_end(css: &str, start: usize, mut end: usize) -> usize {
        while end > start && css.as_bytes()[end - 1].is_ascii_whitespace() {
            end -= 1;
        }
        end
    }
}

impl InspectorStylesheet {
    pub(super) fn position(&self, offset: usize) -> (usize, usize) {
        let prefix = &self.text[..offset.min(self.text.len())];
        let line = prefix.bytes().filter(|byte| *byte == b'\n').count();
        let column = prefix
            .rsplit_once('\n')
            .map_or(prefix.chars().count(), |(_, tail)| tail.chars().count());
        (line, column)
    }

    fn range(&self, start: usize, end: usize) -> Value {
        let start = self.position(start);
        let end = self.position(end);
        json!({
            "startLine": start.0,
            "startColumn": start.1,
            "endLine": end.0,
            "endColumn": end.1,
        })
    }
}

impl Stylesheets<'_> {
    fn inline_declarations(node: &Node) -> Vec<(String, String)> {
        node.attrs()
            .into_iter()
            .flatten()
            .find(|attribute| attribute.name.local.as_ref() == "style")
            .map(|attribute| {
                attribute
                    .value
                    .split(';')
                    .filter_map(|declaration| declaration.split_once(':'))
                    .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
                    .filter(|(name, _)| !name.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(super) fn inline_style(node: &Node) -> Value {
        let declarations = Self::inline_declarations(node);
        let css_text = declarations
            .iter()
            .map(|(name, value)| format!("{name}: {value};"))
            .collect::<Vec<_>>()
            .join(" ");
        let css_properties = declarations
        .into_iter()
        .map(|(name, value)| json!({"name": name, "value": value, "important": false, "implicit": false, "disabled": false}))
        .collect::<Vec<_>>();
        json!({"cssProperties": css_properties, "shorthandEntries": [], "cssText": css_text})
    }

    pub(super) fn matched_styles(&self, node: &Node) -> Vec<Value> {
        let mut matched = Vec::new();
        for sheet in self.all() {
            for rule in CssParser::parse_rules(&sheet.text) {
                let matching_selectors = self.matching_selector_indexes(node, &rule);
                if matching_selectors.is_empty() {
                    continue;
                }
                matched.push(json!({
                    "rule": sheet.protocol_rule(&rule),
                    "matchingSelectors": matching_selectors,
                }));
            }
        }
        matched
    }

    fn matching_selector_indexes(&self, node: &Node, rule: &ParsedRule) -> Vec<usize> {
        rule.selectors
            .iter()
            .enumerate()
            .filter_map(|(index, (selector, _, _))| {
                self.document
                    .query_selector_all(selector)
                    .ok()
                    .is_some_and(|nodes| nodes.contains(&node.id))
                    .then_some(index)
            })
            .collect()
    }
}

impl InspectorStylesheet {
    fn protocol_rule(&self, rule: &ParsedRule) -> Value {
        let selectors = rule
            .selectors
            .iter()
            .map(|(selector, start, end)| {
                json!({
                    "text": selector,
                    "range": self.range(*start, *end),
                })
            })
            .collect::<Vec<_>>();
        json!({
            "styleSheetId": self.id,
            "selectorList": {
                "selectors": selectors,
                "text": rule.selector_text,
                "range": self.range(rule.selector_start, rule.selector_end),
            },
            "origin": "regular",
            "style": self.protocol_style(rule),
            "media": [],
            "containerQueries": [],
            "supports": [],
            "layers": [],
            "scopes": [],
        })
    }

    fn protocol_style(&self, rule: &ParsedRule) -> Value {
        let properties = rule
            .declarations
            .iter()
            .map(|declaration| {
                json!({
                    "name": declaration.name,
                    "value": declaration.value,
                    "important": declaration.important,
                    "implicit": false,
                    "disabled": false,
                    "text": self.text[declaration.start..declaration.end].trim(),
                    "range": self.range(declaration.start, declaration.end),
                })
            })
            .collect::<Vec<_>>();
        json!({
            "styleSheetId": self.id,
            "cssProperties": properties,
            "shorthandEntries": [],
            "cssText": self.text[rule.body_start..rule.body_end].trim(),
            "range": self.range(rule.body_start, rule.body_end),
        })
    }
}

impl Stylesheets<'_> {
    pub(super) fn computed_style(&self, node: &Node) -> Vec<Value> {
        let mut properties = Vec::<(String, String)>::new();
        for sheet in self.all() {
            for rule in CssParser::parse_rules(&sheet.text) {
                if self.matching_selector_indexes(node, &rule).is_empty() {
                    continue;
                }
                for declaration in rule.declarations {
                    Self::set_property(&mut properties, declaration.name, declaration.value);
                }
            }
        }
        for (name, value) in Self::inline_declarations(node) {
            Self::set_property(&mut properties, name, value);
        }
        let layout = node.final_layout();
        let styles = node.primary_styles();
        let layout_properties = [
            (
                "display",
                styles
                    .as_ref()
                    .map(|styles| format!("{:?}", styles.clone_display()).to_ascii_lowercase())
                    .unwrap_or_default(),
            ),
            (
                "position",
                styles
                    .as_ref()
                    .map(|styles| format!("{:?}", styles.clone_position()).to_ascii_lowercase())
                    .unwrap_or_default(),
            ),
            ("width", format!("{}px", layout.content_box_width())),
            ("height", format!("{}px", layout.content_box_height())),
            ("margin-top", format!("{}px", layout.margin.top)),
            ("margin-right", format!("{}px", layout.margin.right)),
            ("margin-bottom", format!("{}px", layout.margin.bottom)),
            ("margin-left", format!("{}px", layout.margin.left)),
            ("padding-top", format!("{}px", layout.padding.top)),
            ("padding-right", format!("{}px", layout.padding.right)),
            ("padding-bottom", format!("{}px", layout.padding.bottom)),
            ("padding-left", format!("{}px", layout.padding.left)),
            ("border-top-width", format!("{}px", layout.border.top)),
            ("border-right-width", format!("{}px", layout.border.right)),
            ("border-bottom-width", format!("{}px", layout.border.bottom)),
            ("border-left-width", format!("{}px", layout.border.left)),
        ];
        for (name, value) in layout_properties {
            Self::set_property(&mut properties, name.into(), value);
        }
        properties
            .into_iter()
            .map(|(name, value)| json!({"name": name, "value": value}))
            .collect()
    }

    fn set_property(properties: &mut Vec<(String, String)>, name: String, value: String) {
        if let Some(property) = properties
            .iter_mut()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(&name))
        {
            property.1 = value;
        } else {
            properties.push((name, value));
        }
    }
}
