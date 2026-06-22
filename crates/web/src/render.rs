//! Imperative canvas rendering of the game board. Kept off the Yew vdom so the
//! 41x41 grid draws fast; a component just calls [`render_game`] from an effect.

use herdcore_core::GameState;
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement};

/// LCD backlight green — the page and canvas background.
const LCD_LIGHT: &str = "#9bbc0f";
/// Grass field the game is played on (darker green for sprite contrast).
const FIELD: &str = "#22401a";
const FIELD_DOT: &str = "#2c4e1f";
const INK: &str = "#0f380f";
const SHEEP_LIGHT: &str = "#cfe07a";

/// Greens used to tell players apart while staying inside a 1-bit green palette.
const SEAT_SHADES: [&str; 8] = [
    "#0f380f", "#306230", "#557a14", "#83a012", "#23491a", "#638716", "#739416", "#456b16",
];

pub fn render_game(
    canvas: &HtmlCanvasElement,
    game: &GameState,
    _my_seat: Option<u32>,
) -> Result<(), JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("window unavailable"))?;
    let viewport_width = window.inner_width()?.as_f64().unwrap_or(900.0);
    let viewport_height = window.inner_height()?.as_f64().unwrap_or(900.0);
    // Reserve vertical room for the title, status, HUD and D-pad, then take the
    // largest square that fits the remaining height and the full width — so the
    // board runs edge-to-edge on a phone and stays square everywhere.
    const CHROME: f64 = 300.0;
    let available_height = (viewport_height - CHROME).max(200.0);
    let board_size = viewport_width.min(available_height).max(200.0);
    let ratio = window.device_pixel_ratio().max(1.0);
    canvas.set_width((board_size * ratio) as u32);
    canvas.set_height((board_size * ratio) as u32);
    canvas.set_attribute("style", &format!("width:{board_size}px;height:{board_size}px"))?;
    let context: CanvasRenderingContext2d = canvas
        .get_context("2d")?
        .ok_or_else(|| JsValue::from_str("2d context unavailable"))?
        .dyn_into()?;
    context.set_transform(ratio, 0.0, 0.0, ratio, 0.0, 0.0)?;
    context.set_image_smoothing_enabled(false);

    let cell = board_size / f64::from(game.width);

    // Field with a faint dot-matrix grid.
    fill(&context, FIELD);
    context.fill_rect(0.0, 0.0, board_size, board_size);
    fill(&context, FIELD_DOT);
    let dot = (cell * 0.12).clamp(0.5, 2.0);
    for y in 0..game.height {
        for x in 0..game.width {
            context.fill_rect(
                f64::from(x) * cell + cell / 2.0 - dot / 2.0,
                f64::from(y) * cell + cell / 2.0 - dot / 2.0,
                dot,
                dot,
            );
        }
    }

    // Pens, tinted by their owner's seat shade.
    for (index, player) in game.players.iter().enumerate() {
        let shade = SEAT_SHADES[index % SEAT_SHADES.len()];
        for pos in &player.pen {
            let px = f64::from(pos.x) * cell;
            let py = f64::from(pos.y) * cell;
            fill(&context, FIELD_DOT);
            context.fill_rect(px, py, cell, cell);
            stroke(&context, shade);
            context.set_line_width((cell * 0.12).clamp(1.0, 3.0));
            context.stroke_rect(px + 1.0, py + 1.0, cell - 2.0, cell - 2.0);
        }
    }

    // Rocks.
    for rock in &game.rocks {
        let px = f64::from(rock.x) * cell;
        let py = f64::from(rock.y) * cell;
        fill(&context, INK);
        context.fill_rect(px + cell * 0.2, py + cell * 0.2, cell * 0.6, cell * 0.6);
    }

    // Sheep.
    for sheep in &game.sheep {
        let px = f64::from(sheep.x) * cell;
        let py = f64::from(sheep.y) * cell;
        draw_sprite(&context, &SHEEP_SPRITE, px, py, cell, |ch| match ch {
            'F' => Some(SHEEP_LIGHT),
            'D' => Some(INK),
            _ => None,
        });
    }

    // Dogs.
    context.set_text_align("center");
    context.set_text_baseline("middle");
    for (index, player) in game.players.iter().enumerate() {
        let px = f64::from(player.dog.x) * cell;
        let py = f64::from(player.dog.y) * cell;
        let shade = SEAT_SHADES[index % SEAT_SHADES.len()];
        draw_sprite(&context, &DOG_SPRITE, px, py, cell, |ch| match ch {
            'B' => Some(shade),
            'D' => Some(INK),
            'W' => Some(LCD_LIGHT),
            _ => None,
        });
        if cell >= 16.0 {
            context.set_font(&format!("{}px 'Press Start 2P', monospace", (cell * 0.3).round()));
            fill(&context, LCD_LIGHT);
            let _ = context.fill_text(
                &(player.seat + 1).to_string(),
                px + cell / 2.0,
                py + cell * 0.92,
            );
        }
    }
    Ok(())
}

/// 8x8 sprites. `D` dark ink, `F` light fleece, `B` body (seat tint), `W` light
/// highlight, `.` transparent.
const SHEEP_SPRITE: [&str; 8] = [
    "..FFFF..",
    ".FFFFFF.",
    "FFFFFFFF",
    "FFDFFDFF",
    "FFFFFFFF",
    ".FFFFFF.",
    ".D....D.",
    ".D....D.",
];

const DOG_SPRITE: [&str; 8] = [
    "DD....DD",
    "DBD..DBD",
    "DBBBBBBD",
    "DBWBBWBD",
    "DBBBBBBD",
    "DBBDDBBD",
    ".DBBBBD.",
    "..D..D..",
];

fn draw_sprite<F>(
    context: &CanvasRenderingContext2d,
    sprite: &[&str; 8],
    x0: f64,
    y0: f64,
    cell: f64,
    color_of: F,
) where
    F: Fn(char) -> Option<&'static str>,
{
    let px = cell / 8.0;
    for (row, line) in sprite.iter().enumerate() {
        for (col, ch) in line.chars().enumerate() {
            if let Some(color) = color_of(ch) {
                fill(context, color);
                context.fill_rect(x0 + col as f64 * px, y0 + row as f64 * px, px + 0.6, px + 0.6);
            }
        }
    }
}

#[allow(deprecated)]
fn fill(context: &CanvasRenderingContext2d, color: &str) {
    context.set_fill_style_str(color);
}

#[allow(deprecated)]
fn stroke(context: &CanvasRenderingContext2d, color: &str) {
    context.set_stroke_style_str(color);
}
