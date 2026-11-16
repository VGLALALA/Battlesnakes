//! Translation between Battlesnake HTTP payloads and [`State`].

use crate::json::{self, Json};
use crate::state::{Cell, Dir, Rules, State, DIRS, MAX_CELLS, MAX_SNAKES, UP};

#[derive(Clone)]
pub struct Request {
    pub rules: Rules,
    pub state: State,
    /// Index of our own snake in the state.
    pub me: usize,
    /// Per-move deadline the engine advertises, in milliseconds.
    pub timeout_ms: i64,
    pub turn: i32,
}

/// Parses a `/move` payload.
///
/// Snakes keep the order the board lists them in, so indices are stable for
/// the whole request. `you` is always among `board.snakes`, and its position in
/// that list becomes [`Request::me`].
pub fn parse_request(body: &str) -> Result<Request, String> {
    let root = json::parse(body)?;
    let board = root.get("board").ok_or("missing board")?;

    let width = board.get("width").and_then(Json::as_i64).ok_or("missing width")? as u8;
    let height = board
        .get("height")
        .and_then(Json::as_i64)
        .ok_or("missing height")? as u8;
    if width == 0 || height == 0 {
        return Err("degenerate board".to_string());
    }
    if width as usize * height as usize > MAX_CELLS {
        return Err(format!("board {}x{} is larger than supported", width, height));
    }

    let snakes = board
        .get("snakes")
        .and_then(Json::as_arr)
        .ok_or("missing snakes")?;
    if snakes.is_empty() {
        return Err("no snakes on the board".to_string());
    }
    let count = snakes.len().min(MAX_SNAKES);

    let my_id = root
        .get("you")
        .and_then(|you| you.get("id"))
        .and_then(Json::as_str)
        .ok_or("missing you.id")?;

    let ruleset_name = root
        .get("game")
        .and_then(|g| g.get("ruleset"))
        .map(|rs| rs.str_or("name", "standard"))
        .unwrap_or("standard")
        .to_ascii_lowercase();
    let map_name = root
        .get("game")
        .map(|g| g.str_or("map", "standard"))
        .unwrap_or("standard")
        .to_ascii_lowercase();

    let wrapped = ruleset_name.contains("wrapped") || map_name.contains("wrapped");
    let constrictor = ruleset_name.contains("constrictor") || map_name.contains("constrictor");
    let hazard_damage = root.int_or(
        &["game", "ruleset", "settings", "hazardDamagePerTurn"],
        14,
    ) as i16;

    let rules = Rules::new(width, height, count as u8)
        .wrapped(wrapped)
        .constrictor(constrictor)
        .hazard_damage(hazard_damage);

    let turn = root.get("turn").and_then(Json::as_i64).unwrap_or(0) as i32;
    let mut state = State::empty();
    state.turn = turn;

    let mut me = None;
    for (index, snake) in snakes.iter().take(count).enumerate() {
        let id = snake.str_or("id", "");
        if id == my_id {
            me = Some(index);
        }
        let body = snake
            .get("body")
            .and_then(Json::as_arr)
            .ok_or("snake without a body")?;
        if body.is_empty() {
            return Err("snake with an empty body".to_string());
        }
        let mut cells = Vec::with_capacity(body.len());
        for point in body {
            cells.push(read_point(point, &rules)?);
        }
        let health = snake.get("health").and_then(Json::as_i64).unwrap_or(100) as i16;
        state.place_snake(index, &cells, health);
    }
    let me = me.ok_or("you.id does not match any snake on the board")?;

    if let Some(food) = board.get("food").and_then(Json::as_arr) {
        for point in food {
            state.food.insert(read_point(point, &rules)?);
        }
    }
    if let Some(hazards) = board.get("hazards").and_then(Json::as_arr) {
        for point in hazards {
            state.hazard.insert(read_point(point, &rules)?);
        }
    }

    let timeout_ms = root.int_or(&["game", "timeout"], 500);

    Ok(Request {
        rules,
        state,
        me,
        timeout_ms,
        turn,
    })
}

fn read_point(point: &Json, rules: &Rules) -> Result<Cell, String> {
    let x = point.get("x").and_then(Json::as_i64).ok_or("point without x")?;
    let y = point.get("y").and_then(Json::as_i64).ok_or("point without y")?;
    if x < 0 || y < 0 || x >= rules.width as i64 || y >= rules.height as i64 {
        return Err(format!("point ({}, {}) is off the board", x, y));
    }
    Ok(rules.cell(x as i32, y as i32))
}

/// A best-effort move derived straight from the payload, for the cases where
/// the state cannot be built at all (an unsupported board size, or a malformed
/// request). Walks the raw JSON, avoids walls and every body segment, and
/// prefers food. Returning *something* survivable beats returning a default.
pub fn fallback_move(body: &str) -> Dir {
    let Ok(root) = json::parse(body) else {
        return UP;
    };
    let Some(board) = root.get("board") else {
        return UP;
    };
    let width = board.get("width").and_then(Json::as_i64).unwrap_or(11);
    let height = board.get("height").and_then(Json::as_i64).unwrap_or(11);

    let point = |p: &Json| -> Option<(i64, i64)> {
        Some((p.get("x")?.as_i64()?, p.get("y")?.as_i64()?))
    };

    let mut blocked: Vec<(i64, i64)> = Vec::new();
    if let Some(snakes) = board.get("snakes").and_then(Json::as_arr) {
        for snake in snakes {
            if let Some(segments) = snake.get("body").and_then(Json::as_arr) {
                blocked.extend(segments.iter().filter_map(point));
            }
        }
    }
    let mut foods: Vec<(i64, i64)> = Vec::new();
    if let Some(items) = board.get("food").and_then(Json::as_arr) {
        foods.extend(items.iter().filter_map(point));
    }

    let head = root
        .get("you")
        .and_then(|you| you.get("head"))
        .and_then(point)
        .unwrap_or((width / 2, height / 2));

    let mut best = UP;
    let mut best_score = i64::MIN;
    for &d in &DIRS {
        let (dx, dy) = match d {
            crate::state::UP => (0, 1),
            crate::state::DOWN => (0, -1),
            crate::state::LEFT => (-1, 0),
            _ => (1, 0),
        };
        let target = (head.0 + dx, head.1 + dy);
        if target.0 < 0 || target.1 < 0 || target.0 >= width || target.1 >= height {
            continue;
        }
        if blocked.contains(&target) {
            continue;
        }
        let mut score = 100;
        if foods.contains(&target) {
            score += 10;
        }
        // Edge away from the walls, which is where the reflex agent dies.
        score -= (target.0 - width / 2).abs() + (target.1 - height / 2).abs();
        if score > best_score {
            best_score = score;
            best = d;
        }
    }
    best
}

pub struct Info {
    pub author: &'static str,
    pub color: &'static str,
    pub head: &'static str,
    pub tail: &'static str,
    pub version: &'static str,
}

impl Info {
    pub fn to_json(&self) -> String {
        format!(
            r#"{{"apiversion":"1","author":"{}","color":"{}","head":"{}","tail":"{}","version":"{}"}}"#,
            json::escape(self.author),
            json::escape(self.color),
            json::escape(self.head),
            json::escape(self.tail),
            json::escape(self.version),
        )
    }
}

pub fn move_json(dir: Dir, shout: &str) -> String {
    format!(
        r#"{{"move":"{}","shout":"{}"}}"#,
        crate::state::dir_name(dir),
        json::escape(shout)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{DOWN, LEFT, RIGHT};

    const SAMPLE: &str = r#"{
      "game": {
        "id": "g1",
        "ruleset": {"name": "royale", "version": "v1.2.3",
                    "settings": {"foodSpawnChance": 15, "minimumFood": 1,
                                 "hazardDamagePerTurn": 20}},
        "map": "royale",
        "timeout": 500,
        "source": "league"
      },
      "turn": 7,
      "board": {
        "width": 11, "height": 11,
        "food": [{"x": 5, "y": 5}, {"x": 9, "y": 0}],
        "hazards": [{"x": 0, "y": 0}],
        "snakes": [
          {"id": "other", "name": "Them", "health": 60,
           "body": [{"x": 8, "y": 8}, {"x": 8, "y": 9}, {"x": 8, "y": 10}],
           "head": {"x": 8, "y": 8}, "length": 3},
          {"id": "mine", "name": "Us", "health": 90,
           "body": [{"x": 2, "y": 2}, {"x": 2, "y": 1}, {"x": 2, "y": 0}],
           "head": {"x": 2, "y": 2}, "length": 3}
        ]
      },
      "you": {"id": "mine", "name": "Us", "health": 90,
              "body": [{"x": 2, "y": 2}, {"x": 2, "y": 1}, {"x": 2, "y": 0}],
              "head": {"x": 2, "y": 2}, "length": 3}
    }"#;

    #[test]
    fn parses_a_full_move_request() {
        let req = parse_request(SAMPLE).expect("parses");
        assert_eq!(req.me, 1, "our snake is second in the board list");
        assert_eq!(req.turn, 7);
        assert_eq!(req.timeout_ms, 500);
        assert_eq!(req.rules.width, 11);
        assert_eq!(req.rules.n, 2);
        assert_eq!(req.rules.hazard_damage, 20);
        assert!(!req.rules.wrapped && !req.rules.constrictor);

        assert_eq!(req.state.head[1], req.rules.cell(2, 2));
        assert_eq!(req.state.len[1], 3);
        assert_eq!(req.state.health[1], 90);
        assert_eq!(req.state.health[0], 60);
        assert_eq!(req.state.alive_count(), 2);
        assert!(req.state.food.contains(req.rules.cell(5, 5)));
        assert!(req.state.food.contains(req.rules.cell(9, 0)));
        assert!(req.state.hazard.contains(req.rules.cell(0, 0)));
        assert!(req.state.occupied(req.rules.cell(2, 0)), "tail is on the board");
    }

    #[test]
    fn detects_game_modes_from_ruleset_or_map() {
        let wrapped = SAMPLE.replace(r#""name": "royale""#, r#""name": "wrapped""#);
        assert!(parse_request(&wrapped).unwrap().rules.wrapped);

        let constrictor = SAMPLE.replace(r#""map": "royale""#, r#""map": "constrictor""#);
        assert!(parse_request(&constrictor).unwrap().rules.constrictor);
    }

    #[test]
    fn defaults_hazard_damage_when_settings_are_absent() {
        let stripped = SAMPLE.replace(r#""hazardDamagePerTurn": 20"#, r#""unrelated": 1"#);
        assert_eq!(parse_request(&stripped).unwrap().rules.hazard_damage, 14);
    }

    #[test]
    fn rejects_payloads_it_cannot_represent() {
        assert!(parse_request("not json").is_err());
        assert!(parse_request(r#"{"board":{}}"#).is_err());

        let oversize = SAMPLE
            .replace(r#""width": 11"#, r#""width": 40"#)
            .replace(r#""height": 11"#, r#""height": 40"#);
        assert!(parse_request(&oversize).is_err(), "40x40 exceeds MAX_CELLS");

        let orphan = SAMPLE.replace(r#""you": {"id": "mine""#, r#""you": {"id": "ghost""#);
        assert!(parse_request(&orphan).is_err(), "you.id must match a snake");
    }

    #[test]
    fn rejects_points_off_the_board() {
        let bad = SAMPLE.replace(r#"{"x": 5, "y": 5}"#, r#"{"x": 50, "y": 5}"#);
        assert!(parse_request(&bad).is_err());
    }

    #[test]
    fn stacked_opening_body_is_understood() {
        let opening = r#"{
          "game": {"id":"g","ruleset":{"name":"standard"},"timeout":500},
          "turn": 0,
          "board": {"width":11,"height":11,"food":[],"hazards":[],
            "snakes":[{"id":"mine","health":100,
              "body":[{"x":1,"y":1},{"x":1,"y":1},{"x":1,"y":1}],
              "head":{"x":1,"y":1},"length":3}]},
          "you": {"id":"mine","health":100,
            "body":[{"x":1,"y":1},{"x":1,"y":1},{"x":1,"y":1}],
            "head":{"x":1,"y":1},"length":3}
        }"#;
        let req = parse_request(opening).expect("parses");
        assert_eq!(req.state.len[0], 3);
        assert_eq!(req.state.dupes[0], 2, "three segments stacked on one cell");
    }

    #[test]
    fn fallback_avoids_walls_and_bodies() {
        // Head in the bottom-left corner with its body running right.
        let payload = r#"{
          "board":{"width":11,"height":11,"food":[],"hazards":[],
            "snakes":[{"id":"mine","body":[{"x":0,"y":0},{"x":1,"y":0},{"x":2,"y":0}]}]},
          "you":{"id":"mine","head":{"x":0,"y":0},
                 "body":[{"x":0,"y":0},{"x":1,"y":0},{"x":2,"y":0}]}
        }"#;
        assert_eq!(fallback_move(payload), UP, "down and left are walls, right is body");
    }

    #[test]
    fn fallback_survives_garbage() {
        assert_eq!(fallback_move("}{"), UP);
        assert_eq!(fallback_move("{}"), UP);
    }

    #[test]
    fn renders_responses() {
        assert_eq!(
            move_json(LEFT, "hi"),
            r#"{"move":"left","shout":"hi"}"#
        );
        assert_eq!(move_json(DOWN, ""), r#"{"move":"down","shout":""}"#);
        assert_eq!(move_json(RIGHT, ""), r#"{"move":"right","shout":""}"#);
        let info = Info {
            author: "a",
            color: "#112233",
            head: "default",
            tail: "default",
            version: "1.0.0",
        };
        let rendered = info.to_json();
        assert!(rendered.contains(r#""apiversion":"1""#));
        assert!(rendered.contains(r##""color":"#112233""##));
    }
}
