use std::collections::{BTreeMap, BTreeSet};

use roxmltree::{Document, Node, NodeType};
use svgtypes::{PathParser, PathSegment};

use super::super::{PpError, PpResult, SvgContract};
use super::ir::{
    PaintFacts, PathCommand, PathFacts, SvgAttributeRange, SvgElement, SvgIr, SvgLimits, SvgRoot,
    Transform,
};

const SVG_NAMESPACE: &str = "http://www.w3.org/2000/svg";
const MAX_CANVAS_DIMENSION: u32 = 8_192;
const MAX_COORDINATE_MAGNITUDE: f64 = 1_000_000.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ParseMode {
    Static,
    GeneratedMotion,
}

impl SvgContract {
    /// Validates only the bounded SVG subset plus motion's generated wrapper groups and stylesheet.
    pub fn validate_generated_motion(svg: &str) -> PpResult<()> {
        let limits = SvgLimits::default().clamped_to_hard_maxima();
        if svg.trim().is_empty() {
            return Err(contract_error("empty SVG output"));
        }
        if svg.len() > limits.max_bytes {
            return Err(limit_error("bytes"));
        }
        reject_unsafe_xml_markup(svg)?;
        let document = Document::parse(svg)
            .map_err(|error| contract_error(format!("malformed SVG XML: {error}")))?;
        let root = document.root_element();
        if root.tag_name().name() != "svg" || root.tag_name().namespace() != Some(SVG_NAMESPACE) {
            return Err(contract_error(
                "output does not contain an SVG namespace root",
            ));
        }
        let mut parser = Parser::new(svg, limits, ParseMode::GeneratedMotion);
        parser.visit(root, None, 1)?;
        parser.finish().map(|_| ())
    }
}
pub fn parse_bounded(svg: &str, limits: SvgLimits) -> PpResult<SvgIr> {
    let limits = limits.clamped_to_hard_maxima();
    if svg.trim().is_empty() {
        return Err(contract_error("empty SVG output"));
    }
    if svg.len() > limits.max_bytes {
        return Err(limit_error("bytes"));
    }
    reject_unsafe_xml_markup(svg)?;
    let document = Document::parse(svg)
        .map_err(|error| contract_error(format!("malformed SVG XML: {error}")))?;
    let root = document.root_element();
    if root.tag_name().name() != "svg" || root.tag_name().namespace() != Some(SVG_NAMESPACE) {
        return Err(contract_error(
            "output does not contain an SVG namespace root",
        ));
    }
    if document.root().descendants().any(|node| {
        node.is_element()
            && matches!(
                node.tag_name().name().to_ascii_lowercase().as_str(),
                "image" | "feimage"
            )
    }) {
        return Err(contract_error("SVG must not embed raster image payloads"));
    }

    let mut parser = Parser::new(svg, limits, ParseMode::Static);
    parser.visit(root, None, 1)?;
    parser.finish()
}

fn reject_unsafe_xml_markup(svg: &str) -> PpResult<()> {
    if svg.contains("<?") {
        return Err(contract_error(
            "SVG processing instructions are unsupported",
        ));
    }
    if svg.contains('&') {
        return Err(contract_error("SVG entities are unsupported"));
    }
    let mut offset = 0;
    while let Some(relative) = svg[offset..].find("<!") {
        let start = offset + relative;
        if svg[start..].starts_with("<!--") {
            let Some(end) = svg[start + 4..].find("-->") else {
                return Err(contract_error("unterminated SVG comment"));
            };
            offset = start + 4 + end + 3;
        } else {
            return Err(contract_error(
                "SVG declarations, entities, and CDATA are unsupported",
            ));
        }
    }
    Ok(())
}

#[derive(Clone)]
struct Frame {
    fill: Option<String>,
    opacity: f64,
    fill_opacity: f64,
    transform: Transform,
    local_name: &'static str,
}

struct Parser<'a> {
    input: &'a str,
    limits: SvgLimits,
    elements: Vec<SvgElement>,
    ids: BTreeSet<String>,
    references: BTreeSet<String>,
    motion_rules: BTreeSet<String>,
    motion_wrappers: BTreeSet<String>,
    attributes: usize,
    tokens: usize,
    coordinates: usize,
    segments: usize,
    root: Option<SvgRoot>,
    styles: usize,
    mode: ParseMode,
}
impl<'a> Parser<'a> {
    fn new(input: &'a str, limits: SvgLimits, mode: ParseMode) -> Self {
        Self {
            input,
            limits,
            elements: Vec::new(),
            ids: BTreeSet::new(),
            references: BTreeSet::new(),
            motion_rules: BTreeSet::new(),
            motion_wrappers: BTreeSet::new(),
            attributes: 0,
            tokens: 0,
            coordinates: 0,
            segments: 0,
            root: None,
            styles: 0,
            mode,
        }
    }

    fn visit(&mut self, node: Node<'_, 'a>, parent: Option<&Frame>, depth: usize) -> PpResult<()> {
        if depth > self.limits.max_depth {
            return Err(limit_error("depth"));
        }
        let namespace = node.tag_name().namespace();
        if namespace != Some(SVG_NAMESPACE) {
            return Err(contract_error("SVG elements must use the SVG namespace"));
        }
        let name = node.tag_name().name();
        if parent.is_some_and(|frame| frame.local_name == "path" || frame.local_name == "style") {
            return Err(contract_error(
                "SVG terminal elements cannot contain children",
            ));
        }
        if parent.is_some_and(|frame| {
            !matches!(frame.local_name, "svg" | "g")
                || (name == "style" && frame.local_name != "svg")
        }) {
            return Err(contract_error("SVG elements require svg or g containers"));
        }
        if node
            .namespaces()
            .any(|namespace| namespace.uri() != SVG_NAMESPACE)
        {
            return Err(contract_error("SVG contains a foreign namespace"));
        }
        if !(matches!(name, "svg" | "g" | "path")
            || self.mode == ParseMode::GeneratedMotion && name == "style")
        {
            return Err(contract_error(format!("unsupported SVG element '{name}'")));
        }
        if self.root.is_some() && name == "svg" {
            return Err(contract_error("nested SVG elements are unsupported"));
        }

        let opening_tag = opening_tag_range(self.input, node.range().start)?;
        self.attributes += namespace_declaration_count(&self.input[opening_tag.clone()]);
        if self.attributes > self.limits.max_attributes {
            return Err(limit_error("attributes"));
        }
        let mut attributes = BTreeMap::new();
        let mut attribute_ranges = BTreeMap::new();
        for attribute in node.attributes() {
            if attribute.name() == "base"
                && attribute.namespace() == Some("http://www.w3.org/XML/1998/namespace")
            {
                return Err(contract_error("xml:base is unsupported"));
            }
            if attribute.namespace().is_some() {
                return Err(contract_error("namespaced SVG attributes are unsupported"));
            }
            let key = attribute.name();
            if !allowed_attribute(self.mode, name, key) {
                return Err(contract_error(format!(
                    "unsupported SVG attribute '{key}' on '{name}'"
                )));
            }
            if key == "id" && attribute.value().len() > self.limits.max_id_bytes {
                return Err(limit_error("ID bytes"));
            }
            let value = validate_attribute(name, key, attribute.value())?;
            self.attributes += 1;
            if self.attributes > self.limits.max_attributes {
                return Err(limit_error("attributes"));
            }
            self.record_value(key, &value)?;
            attribute_ranges.insert(
                key.to_string(),
                SvgAttributeRange {
                    attribute: attribute.range(),
                    value: attribute.range_value(),
                },
            );
            if attributes.insert(key.to_string(), value).is_some() {
                return Err(contract_error("duplicate SVG attribute"));
            }
        }

        if name == "svg" {
            let width = attributes
                .get("width")
                .map(|value| dimension(value))
                .transpose()?;
            let height = attributes
                .get("height")
                .map(|value| dimension(value))
                .transpose()?;
            if width.is_none() || height.is_none() {
                return Err(contract_error(
                    "SVG canvas requires paired width and height",
                ));
            }
            let view_box = view_box(
                attributes
                    .get("viewBox")
                    .ok_or_else(|| contract_error("SVG canvas requires a viewBox"))?,
            )?;
            let width = width.expect("checked paired SVG canvas dimensions");
            let height = height.expect("checked paired SVG canvas dimensions");
            if view_box != [0.0, 0.0, f64::from(width), f64::from(height)] {
                return Err(contract_error(
                    "SVG viewBox must map exactly to the declared viewport",
                ));
            }
            self.root = Some(SvgRoot {
                width: Some(width),
                height: Some(height),
                view_box: Some(view_box),
                closing_tag: closing_tag_range(self.input, node.range())?,
            });
        }
        if let Some(id) = attributes.get("id") {
            if !self.ids.insert(id.clone()) {
                return Err(contract_error("duplicate SVG ID"));
            }
        }

        if name == "style" {
            self.styles += 1;
            if self.styles > 1 {
                return Err(contract_error(
                    "generated motion SVG must contain one style element",
                ));
            }
            let mut text = None;
            for child in node.children() {
                match child.node_type() {
                    NodeType::Text if text.is_none() => text = child.text(),
                    _ => {
                        return Err(contract_error(
                            "generated motion style must contain one text node",
                        ))
                    }
                }
            }
            self.validate_motion_style(
                text.ok_or_else(|| contract_error("generated motion style is empty"))?,
            )?;
        }
        let path = attributes
            .get("d")
            .map(|data| self.path_facts(data))
            .transpose()?;
        if name == "path" && path.is_none() {
            return Err(contract_error("SVG path requires d attribute"));
        }
        if name == "g" && attributes.contains_key("opacity") {
            return Err(contract_error("SVG group opacity is unsupported"));
        }
        let inherited_opacity = parent.map_or(1.0, |frame| frame.opacity)
            * attributes
                .get("opacity")
                .map_or(Ok(1.0), |value| opacity(value))?;
        let fill = attributes
            .get("fill")
            .cloned()
            .or_else(|| parent.and_then(|frame| frame.fill.clone()))
            .or_else(|| (parent.is_none()).then(|| "#000000".to_string()));
        if name == "path" && fill.as_deref() == Some("none") {
            return Err(contract_error("SVG path has no rendered paint"));
        }
        let local_transform = attributes
            .get("transform")
            .map(|value| transform(value))
            .transpose()?
            .unwrap_or(Transform::IDENTITY);
        let transform = compose(
            parent.map_or(Transform::IDENTITY, |frame| frame.transform),
            local_transform,
        );
        validate_transform(transform)?;
        let fill_opacity = attributes.get("fill-opacity").map_or(
            Ok(parent.map_or(1.0, |frame| frame.fill_opacity)),
            |value| opacity(value),
        )?;
        if name == "path" && inherited_opacity * fill_opacity == 0.0 {
            return Err(contract_error("SVG path has no visible rendered paint"));
        }
        if self.mode == ParseMode::GeneratedMotion && name == "g" {
            if let Some(class) = attributes.get("class") {
                if !is_motion_class(class) {
                    return Err(contract_error("invalid generated motion wrapper class"));
                }
                self.motion_wrappers.insert(class.clone());
                self.references.insert(class.clone());
            }
        }
        self.elements.push(SvgElement {
            local_name: name.to_string(),
            range: node.range(),
            opening_tag,
            attributes,
            attribute_ranges,
            inherited_opacity,
            transform,
            paint: (name == "path").then_some(PaintFacts {
                fill: fill.clone(),
                stroke: None,
                opacity: inherited_opacity * fill_opacity,
            }),
            path,
        });
        if self.elements.len() > self.limits.max_elements {
            return Err(limit_error("elements"));
        }

        let frame = Frame {
            fill,
            opacity: inherited_opacity,
            fill_opacity,
            transform,
            local_name: match name {
                "svg" => "svg",
                "g" => "g",
                "path" => "path",
                "style" => "style",
                _ => unreachable!(),
            },
        };
        for child in node.children() {
            match child.node_type() {
                NodeType::Element => self.visit(child, Some(&frame), depth + 1)?,
                NodeType::Text
                    if name != "style" && !child.text().unwrap_or_default().trim().is_empty() =>
                {
                    return Err(contract_error("SVG text content is unsupported"))
                }
                NodeType::PI => {
                    return Err(contract_error(
                        "SVG processing instructions are unsupported",
                    ))
                }
                NodeType::Root | NodeType::Comment | NodeType::Text => {}
            }
        }
        Ok(())
    }

    fn finish(self) -> PpResult<SvgIr> {
        let Some(root) = self.root else {
            return Err(contract_error("output does not contain an SVG root"));
        };
        if self.mode == ParseMode::GeneratedMotion && self.styles != 1 {
            return Err(contract_error(
                "generated motion SVG must contain one style element",
            ));
        }
        if !self.elements.iter().any(|element| element.path.is_some()) {
            return Err(contract_error("SVG contains no rendered paths"));
        }
        if self.mode == ParseMode::GeneratedMotion && self.motion_wrappers != self.motion_rules {
            return Err(contract_error(
                "generated motion rules must bind exactly to wrapper classes",
            ));
        }
        Ok(SvgIr {
            source: self.input.to_string(),
            root,
            elements: self.elements,
            ids: self.ids,
            references: self.references.into_iter().collect(),
            token_count: self.tokens,
            coordinate_count: self.coordinates,
        })
    }

    fn validate_motion_style(&mut self, css: &str) -> PpResult<()> {
        let facts = validate_generated_motion_css(css)?;
        for id in &facts.references {
            if !self.motion_rules.insert(id.clone()) {
                return Err(contract_error("duplicate generated motion rule"));
            }
            self.references.insert(id.clone());
        }
        self.tokens = self
            .tokens
            .checked_add(facts.tokens)
            .ok_or_else(|| limit_error("tokens"))?;
        self.coordinates = self
            .coordinates
            .checked_add(facts.coordinates)
            .ok_or_else(|| limit_error("coordinates"))?;
        if self.tokens > self.limits.max_tokens {
            return Err(limit_error("tokens"));
        }
        if self.coordinates > self.limits.max_coordinates {
            return Err(limit_error("coordinates"));
        }
        Ok(())
    }

    fn record_value(&mut self, key: &str, value: &str) -> PpResult<()> {
        if key != "d" {
            self.tokens += 1;
        }
        let coordinate_count = match key {
            "width" | "height" | "viewBox" | "opacity" | "fill-opacity" => {
                strict_numbers(value, key)?.len()
            }
            "transform" => {
                self.tokens += value.matches('(').count();
                transform_numeric_count(value)?
            }
            _ => 0,
        };
        self.coordinates += coordinate_count;
        if self.tokens > self.limits.max_tokens {
            return Err(limit_error("tokens"));
        }
        if self.coordinates > self.limits.max_coordinates {
            return Err(limit_error("coordinates"));
        }
        Ok(())
    }

    fn path_facts(&mut self, data: &str) -> PpResult<PathFacts> {
        let mut commands = Vec::new();
        let mut segments = Vec::new();
        let mut position = PathPosition::default();
        validate_path_separators(data)?;
        for segment in PathParser::from(data) {
            let segment = segment
                .map_err(|error| contract_error(format!("invalid SVG path data: {error}")))?;
            validate_absolute_path_segment(&segment, &mut position)?;
            self.coordinates += path_number_count(&segment);
            self.tokens += 1;
            self.segments += 1;
            if self.coordinates > self.limits.max_coordinates {
                return Err(limit_error("coordinates"));
            }
            if self.tokens > self.limits.max_tokens {
                return Err(limit_error("tokens"));
            }
            commands.push(match segment {
                PathSegment::MoveTo { .. } => PathCommand::Move,
                PathSegment::LineTo { .. }
                | PathSegment::HorizontalLineTo { .. }
                | PathSegment::VerticalLineTo { .. } => PathCommand::Line,
                PathSegment::CurveTo { .. }
                | PathSegment::SmoothCurveTo { .. }
                | PathSegment::Quadratic { .. }
                | PathSegment::SmoothQuadratic { .. }
                | PathSegment::EllipticalArc { .. } => PathCommand::Curve,
                PathSegment::ClosePath { .. } => PathCommand::Close,
            });
            segments.push(segment);
            if self.segments > self.limits.max_path_segments {
                return Err(limit_error("path segments"));
            }
        }
        if segments.is_empty() {
            return Err(contract_error("SVG path data is empty"));
        }
        if !commands
            .iter()
            .any(|command| matches!(command, PathCommand::Line | PathCommand::Curve))
        {
            return Err(contract_error("SVG path has no rendered geometry"));
        }
        Ok(PathFacts { commands, segments })
    }
}

fn allowed_attribute(mode: ParseMode, element: &str, attribute: &str) -> bool {
    match element {
        "svg" => matches!(
            attribute,
            "width" | "height" | "viewBox" | "shape-rendering"
        ),
        "g" => {
            matches!(
                attribute,
                "id" | "fill" | "opacity" | "fill-opacity" | "transform"
            ) || (mode == ParseMode::GeneratedMotion && attribute == "class")
        }
        "path" => matches!(
            attribute,
            "id" | "d" | "fill" | "opacity" | "fill-opacity" | "transform"
        ),
        "style" => false,
        _ => false,
    }
}
fn is_motion_class(value: &str) -> bool {
    value.strip_prefix("pp-motion-").is_some_and(|id| {
        !id.is_empty()
            && id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    })
}

struct MotionStyleFacts {
    references: Vec<String>,
    tokens: usize,
    coordinates: usize,
}

fn validate_generated_motion_css(css: &str) -> PpResult<MotionStyleFacts> {
    enum State {
        Rule,
        AwaitKeyframes(String),
        Keyframes { previous_percentage: Option<f64> },
    }

    let mut state = State::Rule;
    let mut tokens = 0;
    let mut coordinates = 0;
    let mut references = BTreeSet::new();
    for line in css.lines().map(str::trim).filter(|line| !line.is_empty()) {
        match &mut state {
            State::Rule => {
                let Some((id, count)) = parse_motion_rule(line)? else {
                    return Err(contract_error(
                        "unsupported generated motion stylesheet rule",
                    ));
                };
                if !references.insert(id.clone()) {
                    return Err(contract_error("duplicate generated motion rule"));
                }
                tokens += count;
                coordinates += count;
                state = State::AwaitKeyframes(id);
            }
            State::AwaitKeyframes(id) if line == format!("@keyframes {id}{{") => {
                state = State::Keyframes {
                    previous_percentage: None,
                };
            }
            State::AwaitKeyframes(_) => {
                return Err(contract_error(
                    "generated motion keyframes must follow their rule",
                ));
            }
            State::Keyframes {
                previous_percentage,
            } if line == "}" => {
                if previous_percentage.is_none() {
                    return Err(contract_error("generated motion keyframes are empty"));
                }
                state = State::Rule;
            }
            State::Keyframes { .. } if line.starts_with("@keyframes ") => {
                return Err(contract_error("unexpected generated motion keyframes"));
            }
            State::Keyframes {
                previous_percentage,
            } => {
                let (percentage, count) = parse_motion_keyframe(line)?;
                if previous_percentage.is_some_and(|previous| percentage <= previous) {
                    return Err(contract_error(
                        "generated motion keyframe percentages must be ordered",
                    ));
                }
                *previous_percentage = Some(percentage);
                tokens += count;
                coordinates += count;
            }
        }
    }
    match state {
        State::Rule if tokens > 0 => Ok(MotionStyleFacts {
            references: references.into_iter().collect(),
            tokens,
            coordinates,
        }),
        _ => Err(contract_error("incomplete generated motion stylesheet")),
    }
}

fn parse_motion_rule(line: &str) -> PpResult<Option<(String, usize)>> {
    let Some(rest) = line.strip_prefix(".pp-motion-") else {
        return Ok(None);
    };
    let Some((id, rest)) = rest.split_once("{transform-box:view-box;transform-origin:") else {
        return Err(contract_error("invalid generated motion rule"));
    };
    let class = format!("pp-motion-{id}");
    if !is_motion_class(&class) {
        return Err(contract_error("invalid generated motion class"));
    }
    let (rest, mut count, _) = motion_number(rest)?;
    let rest = rest
        .strip_prefix("px ")
        .ok_or_else(|| contract_error("invalid generated transform origin"))?;
    let (rest, next_count, _) = motion_number(rest)?;
    count += next_count;
    let rest = rest
        .strip_prefix("px;animation:pp-motion-")
        .ok_or_else(|| contract_error("invalid generated motion rule"))?;
    let rest = rest.strip_prefix(id).ok_or_else(|| {
        contract_error("generated motion animation name does not match its class")
    })?;
    let rest = rest
        .strip_prefix(' ')
        .ok_or_else(|| contract_error("invalid generated motion duration"))?;
    let (duration, rest) = rest
        .split_once("ms ")
        .ok_or_else(|| contract_error("invalid generated motion duration"))?;
    if duration
        .parse::<u32>()
        .ok()
        .filter(|duration| *duration > 0)
        .is_none()
    {
        return Err(contract_error("generated motion duration must be positive"));
    }
    count += 1;
    let (interpolation, repeat) = if let Some(repeat) = rest.strip_prefix("linear ") {
        ("linear", repeat)
    } else if let Some(repeat) = rest.strip_prefix("steps(1,end) ") {
        ("steps(1,end)", repeat)
    } else {
        return Err(contract_error("unsupported generated motion animation"));
    };
    if !matches!(repeat, "infinite;}" | "1 forwards;}") {
        return Err(contract_error("unsupported generated motion animation"));
    }
    if interpolation == "steps(1,end)" {
        count += 1;
    }
    if repeat.starts_with('1') {
        count += 1;
    }
    Ok(Some((class, count)))
}

fn parse_motion_keyframe(line: &str) -> PpResult<(f64, usize)> {
    let (rest, mut count, percentage) = motion_number(line)?;
    if !(0.0..=100.0).contains(&percentage) {
        return Err(contract_error(
            "generated motion keyframe percentage exceeds bounds",
        ));
    }
    let rest = rest
        .strip_prefix("%{transform:translate(")
        .ok_or_else(|| contract_error("invalid generated motion keyframe"))?;
    let (rest, next_count, _) = motion_number(rest)?;
    count += next_count;
    let rest = rest
        .strip_prefix("px,")
        .ok_or_else(|| contract_error("invalid generated motion keyframe"))?;
    let (rest, next_count, _) = motion_number(rest)?;
    count += next_count;
    let rest = rest
        .strip_prefix("px) rotate(")
        .ok_or_else(|| contract_error("invalid generated motion keyframe"))?;
    let (rest, next_count, _) = motion_number(rest)?;
    count += next_count;
    let rest = rest
        .strip_prefix("deg) scale(")
        .ok_or_else(|| contract_error("invalid generated motion keyframe"))?;
    let (rest, next_count, scale_x) = motion_number(rest)?;
    count += next_count;
    let rest = rest
        .strip_prefix(',')
        .ok_or_else(|| contract_error("invalid generated motion keyframe"))?;
    let (rest, next_count, scale_y) = motion_number(rest)?;
    count += next_count;
    if scale_x <= 0.0 || scale_y <= 0.0 {
        return Err(contract_error("generated motion scale must be positive"));
    }
    let rest = rest
        .strip_prefix(");opacity:")
        .ok_or_else(|| contract_error("invalid generated motion keyframe"))?;
    let (rest, next_count, opacity) = motion_number(rest)?;
    count += next_count;
    if !(0.0..=1.0).contains(&opacity) {
        return Err(contract_error("generated motion opacity exceeds bounds"));
    }
    if rest != ";}" {
        return Err(contract_error("invalid generated motion keyframe"));
    }
    Ok((percentage, count))
}

fn motion_number(value: &str) -> PpResult<(&str, usize, f64)> {
    let end = value
        .char_indices()
        .take_while(|(_, ch)| ch.is_ascii_digit() || matches!(ch, '-' | '.'))
        .last()
        .map_or(0, |(index, ch)| index + ch.len_utf8());
    let number = value[..end]
        .parse::<f64>()
        .map_err(|_| contract_error("invalid generated motion number"))?;
    if !number.is_finite() || number.abs() > MAX_COORDINATE_MAGNITUDE {
        return Err(contract_error(
            "generated motion number exceeds coordinate bounds",
        ));
    }
    Ok((&value[end..], 1, number))
}

fn validate_attribute(element: &str, key: &str, value: &str) -> PpResult<String> {
    if value.contains(['<', '>', '\0']) {
        return Err(contract_error("unsafe SVG attribute value"));
    }
    match key {
        "id" => {
            if value.is_empty()
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            {
                return Err(contract_error("invalid SVG ID"));
            }
            Ok(value.to_string())
        }
        "class" if element == "g" && is_motion_class(value) => Ok(value.to_string()),
        "fill" => canonical_paint(value),
        "opacity" | "fill-opacity" => {
            opacity(value)?;
            Ok(value.to_string())
        }
        "transform" => {
            transform(value)?;
            Ok(value.to_string())
        }
        "d" => Ok(value.to_string()),
        "width" | "height" => {
            dimension(value)?;
            Ok(value.to_string())
        }
        "viewBox" => {
            view_box(value)?;
            Ok(value.to_string())
        }
        "shape-rendering" if element == "svg" && value == "crispEdges" => Ok(value.to_string()),
        _ => Err(contract_error("unsupported SVG attribute value")),
    }
}

fn canonical_paint(value: &str) -> PpResult<String> {
    if value == "none" {
        return Ok(value.to_string());
    }
    let hex = value
        .strip_prefix('#')
        .filter(|hex| matches!(hex.len(), 3 | 6))
        .filter(|hex| hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| contract_error("SVG paint must be 'none' or a hexadecimal color"))?;
    let mut canonical = String::with_capacity(7);
    canonical.push('#');
    if hex.len() == 3 {
        for byte in hex.bytes() {
            canonical.push((byte as char).to_ascii_lowercase());
            canonical.push((byte as char).to_ascii_lowercase());
        }
    } else {
        canonical.push_str(&hex.to_ascii_lowercase());
    }
    Ok(canonical)
}

fn opening_tag_range(input: &str, start: usize) -> PpResult<std::ops::Range<usize>> {
    let mut quote = None;
    for (relative, byte) in input[start..].bytes().enumerate() {
        match quote {
            Some(current) if byte == current => quote = None,
            Some(_) => {}
            None if matches!(byte, b'\'' | b'\"') => quote = Some(byte),
            None if byte == b'>' => return Ok(start..start + relative + 1),
            _ => {}
        }
    }
    Err(contract_error("malformed SVG opening tag"))
}
fn closing_tag_range(
    input: &str,
    element_range: std::ops::Range<usize>,
) -> PpResult<std::ops::Range<usize>> {
    let Some(closing_start) = input.as_bytes()[element_range.clone()]
        .iter()
        .rposition(|byte| *byte == b'<')
    else {
        return Err(contract_error("malformed SVG closing tag"));
    };
    let closing_start = element_range.start + closing_start;
    let closing_tag = closing_start..element_range.end;
    if !input[closing_tag.clone()].starts_with("</") {
        return Err(contract_error("malformed SVG closing tag"));
    }
    Ok(closing_tag)
}
fn namespace_declaration_count(opening_tag: &str) -> usize {
    opening_tag
        .split(|ch: char| ch.is_whitespace() || ch == '=')
        .filter(|part| *part == "xmlns" || part.starts_with("xmlns:"))
        .count()
}

fn dimension(value: &str) -> PpResult<u32> {
    let value = value
        .parse::<u32>()
        .map_err(|_| contract_error("SVG dimensions must be unsigned integers"))?;
    if value == 0 || value > MAX_CANVAS_DIMENSION {
        return Err(contract_error(
            "SVG dimensions exceed the supported canvas bounds",
        ));
    }
    Ok(value)
}

fn view_box(value: &str) -> PpResult<[f64; 4]> {
    let values: [f64; 4] = strict_numbers(value, "viewBox")?
        .try_into()
        .map_err(|_| contract_error("invalid SVG viewBox"))?;
    if values
        .iter()
        .any(|value| !value.is_finite() || value.abs() > MAX_COORDINATE_MAGNITUDE)
        || values[2] <= 0.0
        || values[3] <= 0.0
        || values[2] > f64::from(MAX_CANVAS_DIMENSION)
        || values[3] > f64::from(MAX_CANVAS_DIMENSION)
    {
        return Err(contract_error("invalid SVG viewBox"));
    }
    Ok(values)
}

fn opacity(value: &str) -> PpResult<f64> {
    let value = value
        .parse::<f64>()
        .map_err(|_| contract_error("invalid SVG opacity"))?;
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        Err(contract_error("invalid SVG opacity"))
    } else {
        Ok(value)
    }
}

fn strict_numbers(value: &str, kind: &str) -> PpResult<Vec<f64>> {
    let value = value.trim();
    if value.is_empty() {
        return Err(contract_error(format!("invalid SVG {kind}")));
    }
    let mut values = Vec::new();
    for group in value.split(',') {
        let group = group.trim();
        if group.is_empty() {
            return Err(contract_error(format!("invalid SVG {kind} separator")));
        }
        for part in group.split_whitespace() {
            let number = part
                .parse::<f64>()
                .map_err(|_| contract_error(format!("invalid SVG {kind}")))?;
            if !number.is_finite() {
                return Err(contract_error(format!("invalid SVG {kind}")));
            }
            values.push(number);
        }
    }
    Ok(values)
}
fn contract_error(message: impl Into<String>) -> PpError {
    PpError::SvgContract(message.into())
}
fn limit_error(limit: &str) -> PpError {
    contract_error(format!("SVG resource limit exceeded: {limit}"))
}

fn compose(left: Transform, right: Transform) -> Transform {
    let [a, b, c, d, e, f] = left.matrix;
    let [g, h, i, j, k, l] = right.matrix;
    Transform {
        matrix: [
            a * g + c * h,
            b * g + d * h,
            a * i + c * j,
            b * i + d * j,
            a * k + c * l + e,
            b * k + d * l + f,
        ],
        translation_only: left.translation_only && right.translation_only,
    }
}

fn transform(value: &str) -> PpResult<Transform> {
    let mut rest = value.trim();
    let mut result = Transform::IDENTITY;
    while !rest.is_empty() {
        let name_end = rest
            .find('(')
            .ok_or_else(|| contract_error("invalid SVG transform"))?;
        let name = rest[..name_end].trim();
        let after_open = &rest[name_end + 1..];
        let end = after_open
            .find(')')
            .ok_or_else(|| contract_error("invalid SVG transform"))?;
        let values = transform_values(&after_open[..end])?;
        let local = match name {
            "matrix" if values.len() == 6 => Transform {
                matrix: values.try_into().unwrap(),
                translation_only: false,
            },
            "translate" if values.len() == 1 || values.len() == 2 => Transform {
                matrix: [
                    1.0,
                    0.0,
                    0.0,
                    1.0,
                    values[0],
                    *values.get(1).unwrap_or(&0.0),
                ],
                translation_only: true,
            },
            "scale" if values.len() == 1 || values.len() == 2 => Transform {
                matrix: [
                    values[0],
                    0.0,
                    0.0,
                    *values.get(1).unwrap_or(&values[0]),
                    0.0,
                    0.0,
                ],
                translation_only: false,
            },
            "rotate" if values.len() == 1 || values.len() == 3 => {
                let (sin, cos) = values[0].to_radians().sin_cos();
                let rotation = Transform {
                    matrix: [cos, sin, -sin, cos, 0.0, 0.0],
                    translation_only: false,
                };
                if values.len() == 3 {
                    compose(
                        compose(
                            Transform {
                                matrix: [1.0, 0.0, 0.0, 1.0, values[1], values[2]],
                                translation_only: false,
                            },
                            rotation,
                        ),
                        Transform {
                            matrix: [1.0, 0.0, 0.0, 1.0, -values[1], -values[2]],
                            translation_only: false,
                        },
                    )
                } else {
                    rotation
                }
            }
            "skewX" if values.len() == 1 => Transform {
                matrix: [1.0, 0.0, values[0].to_radians().tan(), 1.0, 0.0, 0.0],
                translation_only: false,
            },
            "skewY" if values.len() == 1 => Transform {
                matrix: [1.0, values[0].to_radians().tan(), 0.0, 1.0, 0.0, 0.0],
                translation_only: false,
            },
            _ => return Err(contract_error("invalid SVG transform")),
        };
        validate_transform(local)?;
        result = compose(result, local);
        validate_transform(result)?;
        let tail = &after_open[end + 1..];
        if tail.is_empty() {
            break;
        }
        if !tail
            .chars()
            .next()
            .is_some_and(|ch| ch.is_whitespace() || ch == ',')
        {
            return Err(contract_error("invalid SVG transform separator"));
        }
        rest = tail.trim_start();
        if let Some(stripped) = rest.strip_prefix(',') {
            rest = stripped.trim_start();
            if rest.is_empty() || rest.starts_with(',') {
                return Err(contract_error("invalid SVG transform separator"));
            }
        }
    }
    Ok(result)
}

fn transform_numeric_count(value: &str) -> PpResult<usize> {
    let mut rest = value.trim();
    let mut count = 0;
    while !rest.is_empty() {
        let open = rest
            .find('(')
            .ok_or_else(|| contract_error("invalid SVG transform"))?;
        let close = rest[open + 1..]
            .find(')')
            .ok_or_else(|| contract_error("invalid SVG transform"))?;
        count += strict_numbers(&rest[open + 1..open + 1 + close], "transform")?.len();
        rest = rest[open + close + 2..].trim();
        if let Some(stripped) = rest.strip_prefix(',') {
            rest = stripped.trim();
        }
    }
    Ok(count)
}
fn transform_values(value: &str) -> PpResult<Vec<f64>> {
    let values = strict_numbers(value, "transform")?;
    if values
        .iter()
        .any(|value| value.abs() > MAX_COORDINATE_MAGNITUDE)
    {
        return Err(contract_error("invalid SVG transform"));
    }
    Ok(values)
}

fn validate_path_separators(data: &str) -> PpResult<()> {
    let trimmed = data.trim();
    if trimmed.is_empty()
        || trimmed.starts_with(',')
        || trimmed.ends_with(',')
        || data.contains(",,")
    {
        return Err(contract_error("invalid SVG path separator"));
    }
    Ok(())
}

fn path_number_count(segment: &PathSegment) -> usize {
    match segment {
        PathSegment::MoveTo { .. }
        | PathSegment::LineTo { .. }
        | PathSegment::SmoothQuadratic { .. } => 2,
        PathSegment::HorizontalLineTo { .. } | PathSegment::VerticalLineTo { .. } => 1,
        PathSegment::CurveTo { .. } => 6,
        PathSegment::SmoothCurveTo { .. } | PathSegment::Quadratic { .. } => 4,
        PathSegment::EllipticalArc { .. } => 7,
        PathSegment::ClosePath { .. } => 0,
    }
}

#[derive(Default)]
struct PathPosition {
    current: (f64, f64),
    subpath_start: (f64, f64),
}

fn validate_absolute_path_segment(
    segment: &PathSegment,
    position: &mut PathPosition,
) -> PpResult<()> {
    let relative_point = |abs, x, y| {
        if abs {
            (x, y)
        } else {
            (position.current.0 + x, position.current.1 + y)
        }
    };
    let mut values = Vec::new();
    let endpoint = match *segment {
        PathSegment::MoveTo { abs, x, y } => {
            let endpoint = relative_point(abs, x, y);
            position.subpath_start = endpoint;
            endpoint
        }
        PathSegment::LineTo { abs, x, y } | PathSegment::SmoothQuadratic { abs, x, y } => {
            relative_point(abs, x, y)
        }
        PathSegment::HorizontalLineTo { abs, x } => (
            if abs { x } else { position.current.0 + x },
            position.current.1,
        ),
        PathSegment::VerticalLineTo { abs, y } => (
            position.current.0,
            if abs { y } else { position.current.1 + y },
        ),
        PathSegment::CurveTo {
            abs,
            x1,
            y1,
            x2,
            y2,
            x,
            y,
        } => {
            values.extend([
                relative_point(abs, x1, y1).0,
                relative_point(abs, x1, y1).1,
                relative_point(abs, x2, y2).0,
                relative_point(abs, x2, y2).1,
            ]);
            relative_point(abs, x, y)
        }
        PathSegment::SmoothCurveTo { abs, x2, y2, x, y } => {
            values.extend([relative_point(abs, x2, y2).0, relative_point(abs, x2, y2).1]);
            relative_point(abs, x, y)
        }
        PathSegment::Quadratic { abs, x1, y1, x, y } => {
            values.extend([relative_point(abs, x1, y1).0, relative_point(abs, x1, y1).1]);
            relative_point(abs, x, y)
        }
        PathSegment::EllipticalArc {
            abs,
            rx,
            ry,
            x_axis_rotation,
            x,
            y,
            ..
        } => {
            values.extend([rx, ry, x_axis_rotation]);
            relative_point(abs, x, y)
        }
        PathSegment::ClosePath { .. } => position.subpath_start,
    };
    values.extend([endpoint.0, endpoint.1]);
    if values
        .iter()
        .any(|value| !value.is_finite() || value.abs() > MAX_COORDINATE_MAGNITUDE)
    {
        return Err(contract_error(
            "SVG path coordinate exceeds supported bounds",
        ));
    }
    position.current = endpoint;
    Ok(())
}

fn validate_transform(transform: Transform) -> PpResult<()> {
    if transform
        .matrix
        .iter()
        .any(|value| !value.is_finite() || value.abs() > MAX_COORDINATE_MAGNITUDE)
    {
        Err(contract_error("invalid SVG transform"))
    } else {
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::{parse_bounded, SvgLimits};

    fn two_path_svg() -> &'static str {
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8" viewBox="0 0 8 8"><path fill="#000" d="M0 0L1 1"/><path fill="#000" d="M2 2L3 3"/></svg>"##
    }

    #[test]
    fn path_number_and_segment_limits_are_document_wide() {
        let exact = SvgLimits {
            max_coordinates: 14,
            max_path_segments: 4,
            ..SvgLimits::default()
        };
        assert!(parse_bounded(two_path_svg(), exact).is_ok());

        let coordinate_over_limit = SvgLimits {
            max_coordinates: 13,
            ..exact
        };
        assert!(parse_bounded(two_path_svg(), coordinate_over_limit).is_err());

        let segment_over_limit = SvgLimits {
            max_path_segments: 3,
            ..exact
        };
        assert!(parse_bounded(two_path_svg(), segment_over_limit).is_err());
    }
    #[test]
    fn parser_binds_full_element_spans_to_its_source() {
        let source = r##"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8" viewBox="0 0 8 8"><g><path fill="#000" d="M0 0L1 1"/></g></svg>"##;
        let ir = parse_bounded(source, SvgLimits::default()).unwrap();
        let path = ir
            .elements
            .iter()
            .find(|element| element.local_name == "path")
            .unwrap();
        assert_eq!(ir.source, source);
        assert_eq!(
            &ir.source[path.range.clone()],
            r##"<path fill="#000" d="M0 0L1 1"/>"##
        );
    }

    #[test]
    fn parser_rejects_relative_coordinates_after_accumulation_and_resets_on_close() {
        let over_limit = r##"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8" viewBox="0 0 8 8"><path fill="#000" d="M0 0l1000000 0l1000000 0"/></svg>"##;
        assert!(parse_bounded(over_limit, SvgLimits::default()).is_err());

        let reset = r##"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8" viewBox="0 0 8 8"><path fill="#000" d="M0 0l1000000 0zl1000000 0"/></svg>"##;
        assert!(parse_bounded(reset, SvgLimits::default()).is_ok());
    }
}
