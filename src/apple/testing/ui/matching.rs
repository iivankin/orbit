use serde_json::Value as JsonValue;

use super::{UiCoordinate, UiPointExpr, UiSelector, UiSwipeDirection};

#[derive(Debug, Clone)]
pub(super) struct UiElementMatch {
    pub(super) label: String,
    pub(super) frame: Option<UiFrame>,
    score: u8,
    pub(super) copied_text: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct UiFrame {
    pub(super) x: f64,
    pub(super) y: f64,
    pub(super) width: f64,
    pub(super) height: f64,
}

impl UiFrame {
    pub(super) fn center(self) -> (f64, f64) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }
}

pub(super) fn directional_points_in_frame(
    frame: UiFrame,
    direction: UiSwipeDirection,
    invert_for_scroll: bool,
) -> ((f64, f64), (f64, f64)) {
    let direction = if invert_for_scroll {
        match direction {
            UiSwipeDirection::Left => UiSwipeDirection::Right,
            UiSwipeDirection::Right => UiSwipeDirection::Left,
            UiSwipeDirection::Up => UiSwipeDirection::Down,
            UiSwipeDirection::Down => UiSwipeDirection::Up,
        }
    } else {
        direction
    };

    let left = frame.x + (frame.width * 0.20);
    let right = frame.x + (frame.width * 0.80);
    let top = frame.y + (frame.height * 0.20);
    let bottom = frame.y + (frame.height * 0.80);
    let center_x = frame.x + (frame.width * 0.50);
    let center_y = frame.y + (frame.height * 0.50);

    match direction {
        UiSwipeDirection::Left => ((right, center_y), (left, center_y)),
        UiSwipeDirection::Right => ((left, center_y), (right, center_y)),
        UiSwipeDirection::Up => ((center_x, bottom), (center_x, top)),
        UiSwipeDirection::Down => ((center_x, top), (center_x, bottom)),
    }
}

pub(super) fn resolve_point_expr(screen: &UiFrame, point: &UiPointExpr) -> (f64, f64) {
    (
        resolve_coordinate(screen.x, screen.width, point.x),
        resolve_coordinate(screen.y, screen.height, point.y),
    )
}

pub(super) fn infer_screen_frame(tree: &JsonValue) -> Option<UiFrame> {
    let mut frames = Vec::new();
    collect_frames(tree, &mut frames);
    frames.into_iter().max_by(|left, right| {
        let left_area = left.width * left.height;
        let right_area = right.width * right.height;
        left_area
            .partial_cmp(&right_area)
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

#[cfg(test)]
pub(super) fn find_element_by_selector(
    tree: &JsonValue,
    selector: &UiSelector,
) -> Option<UiElementMatch> {
    let mut matches = Vec::new();
    collect_element_matches(tree, selector, &mut matches);
    select_best_match(matches)
}

pub(super) fn find_visible_element_by_selector(
    tree: &JsonValue,
    selector: &UiSelector,
) -> Option<UiElementMatch> {
    let screen = infer_screen_frame(tree);
    let mut matches = Vec::new();
    collect_element_matches(tree, selector, &mut matches);
    matches.retain(|element| {
        screen.is_none_or(|screen| {
            element
                .frame
                .is_none_or(|frame| frames_intersect(screen, frame))
        })
    });
    select_best_match(matches)
}

pub(super) fn find_visible_scroll_container(tree: &JsonValue) -> Option<UiFrame> {
    let screen = infer_screen_frame(tree);
    let mut frames = Vec::new();
    collect_visible_scroll_frames(tree, screen, &mut frames);
    frames.into_iter().max_by(|left, right| {
        let left_area = left.width * left.height;
        let right_area = right.width * right.height;
        left_area
            .partial_cmp(&right_area)
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

fn select_best_match(mut matches: Vec<UiElementMatch>) -> Option<UiElementMatch> {
    matches.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| right.frame.is_some().cmp(&left.frame.is_some()))
            .then_with(|| left.label.cmp(&right.label))
    });
    matches.into_iter().next()
}

fn resolve_coordinate(origin: f64, span: f64, coordinate: UiCoordinate) -> f64 {
    match coordinate {
        UiCoordinate::Absolute(value) => value,
        UiCoordinate::Percent(percent) => origin + (span * percent / 100.0),
    }
}

fn collect_frames(tree: &JsonValue, frames: &mut Vec<UiFrame>) {
    match tree {
        JsonValue::Array(values) => {
            for value in values {
                collect_frames(value, frames);
            }
        }
        JsonValue::Object(map) => {
            if let Some(frame) = extract_frame(map) {
                frames.push(frame);
            }
            for value in map.values() {
                collect_frames(value, frames);
            }
        }
        _ => {}
    }
}

fn collect_visible_scroll_frames(
    tree: &JsonValue,
    screen: Option<UiFrame>,
    frames: &mut Vec<UiFrame>,
) {
    match tree {
        JsonValue::Array(values) => {
            for value in values {
                collect_visible_scroll_frames(value, screen, frames);
            }
        }
        JsonValue::Object(map) => {
            if let Some(frame) = extract_frame(map)
                && frame.width > 1.0
                && frame.height > 1.0
                && screen
                    .map(|screen| frames_intersect(screen, frame))
                    .unwrap_or(true)
                && map
                    .get("AXRole")
                    .and_then(JsonValue::as_str)
                    .is_some_and(is_scrollable_role)
            {
                frames.push(frame);
            }
            for value in map.values() {
                collect_visible_scroll_frames(value, screen, frames);
            }
        }
        _ => {}
    }
}

fn is_scrollable_role(role: &str) -> bool {
    matches!(
        role,
        "AXScrollArea"
            | "AXScrollView"
            | "AXTable"
            | "AXOutline"
            | "AXList"
            | "AXCollectionView"
            | "XCUIElementTypeCollectionView"
            | "XCUIElementTypeScrollView"
            | "XCUIElementTypeTable"
    )
}

fn collect_element_matches(
    tree: &JsonValue,
    selector: &UiSelector,
    matches: &mut Vec<UiElementMatch>,
) {
    match tree {
        JsonValue::Array(values) => {
            for value in values {
                collect_element_matches(value, selector, matches);
            }
        }
        JsonValue::Object(map) => {
            if let Some(element) = match_element_object(map, selector) {
                matches.push(element);
            }
            for value in map.values() {
                collect_element_matches(value, selector, matches);
            }
        }
        _ => {}
    }
}

fn match_element_object(
    map: &serde_json::Map<String, JsonValue>,
    selector: &UiSelector,
) -> Option<UiElementMatch> {
    let text_candidates = ["AXLabel", "label", "title", "name", "value", "AXValue"];
    let id_candidates = ["identifier", "AXIdentifier", "id"];

    let (text_score, text_label) = selector
        .text
        .as_deref()
        .map(|needle| best_match_for_keys(map, &text_candidates, needle))
        .unwrap_or((1, None));
    if selector.text.is_some() && text_score == 0 {
        return None;
    }
    let (id_score, id_label) = selector
        .id
        .as_deref()
        .map(|needle| best_match_for_keys(map, &id_candidates, needle))
        .unwrap_or((1, None));
    if selector.id.is_some() && id_score == 0 {
        return None;
    }
    let score = text_score.saturating_add(id_score);
    if score == 0 {
        return None;
    }
    let copied_text = preferred_copy_text(map);
    let label = copied_text
        .clone()
        .or(text_label)
        .or(id_label)
        .or_else(|| selector.text.clone())
        .or_else(|| selector.id.clone())
        .unwrap_or_else(|| selector.summary());

    Some(UiElementMatch {
        label,
        frame: extract_frame(map),
        score,
        copied_text,
    })
}

fn match_score(value: &str, needle: &str) -> u8 {
    if value == needle {
        return 3;
    }
    if value.eq_ignore_ascii_case(needle) {
        return 2;
    }
    if value
        .to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
    {
        return 1;
    }
    0
}

fn best_match_for_keys(
    map: &serde_json::Map<String, JsonValue>,
    keys: &[&str],
    needle: &str,
) -> (u8, Option<String>) {
    let mut best_label = None;
    let mut best_score = 0;
    for key in keys {
        let Some(value) = map.get(*key).and_then(JsonValue::as_str) else {
            continue;
        };
        let score = match_score(value, needle);
        if score > best_score {
            best_score = score;
            best_label = Some(value.to_owned());
        }
    }
    (best_score, best_label)
}

fn preferred_copy_text(map: &serde_json::Map<String, JsonValue>) -> Option<String> {
    ["AXValue", "value", "AXLabel", "label", "title", "name"]
        .into_iter()
        .find_map(|key| map.get(key).and_then(JsonValue::as_str).map(str::to_owned))
}

fn extract_frame(map: &serde_json::Map<String, JsonValue>) -> Option<UiFrame> {
    if let Some(frame) = map.get("frame").and_then(json_value_to_frame) {
        return Some(frame);
    }
    if let Some(frame) = map.get("rect").and_then(json_value_to_frame) {
        return Some(frame);
    }
    if let Some(origin) = map.get("origin").and_then(JsonValue::as_object)
        && let Some(size) = map.get("size").and_then(JsonValue::as_object)
    {
        return Some(UiFrame {
            x: json_number(origin.get("x")?)?,
            y: json_number(origin.get("y")?)?,
            width: json_number(size.get("width")?)?,
            height: json_number(size.get("height")?)?,
        });
    }
    None
}

fn frames_intersect(left: UiFrame, right: UiFrame) -> bool {
    let left_max_x = left.x + left.width;
    let left_max_y = left.y + left.height;
    let right_max_x = right.x + right.width;
    let right_max_y = right.y + right.height;
    left.x < right_max_x && left_max_x > right.x && left.y < right_max_y && left_max_y > right.y
}

fn json_value_to_frame(value: &JsonValue) -> Option<UiFrame> {
    let map = value.as_object()?;
    Some(UiFrame {
        x: json_number(map.get("x")?)?,
        y: json_number(map.get("y")?)?,
        width: json_number(map.get("width")?)?,
        height: json_number(map.get("height")?)?,
    })
}

fn json_number(value: &JsonValue) -> Option<f64> {
    value.as_f64().or_else(|| value.as_str()?.parse().ok())
}
