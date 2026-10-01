//! Port of senpi `packages/coding-agent/src/modes/interactive/components/armin.ts`.
//!
//! senpi picks the effect and drives the animation with `Math.random` + `setInterval`; this port
//! takes an explicit effect and seed and advances from host ticks (`tick()`), so a run is
//! reproducible.

use maho_tui::tui::Component;

use crate::theme::{Theme, ThemeColor};

const WIDTH: usize = 31;
const HEIGHT: usize = 36;
const BYTES_PER_ROW: usize = WIDTH.div_ceil(8);
const DISPLAY_HEIGHT: usize = HEIGHT.div_ceil(2);

const BITS: [u8; 144] = [
    0xff, 0xff, 0xff, 0x7f, 0xff, 0xf0, 0xff, 0x7f, 0xff, 0xed, 0xff, 0x7f,
    0xff, 0xdb, 0xff, 0x7f, 0xff, 0xb7, 0xff, 0x7f, 0xff, 0x77, 0xfe, 0x7f,
    0x3f, 0xf8, 0xfe, 0x7f, 0xdf, 0xff, 0xfe, 0x7f, 0xdf, 0x3f, 0xfc, 0x7f,
    0x9f, 0xc3, 0xfb, 0x7f, 0x6f, 0xfc, 0xf4, 0x7f, 0xf7, 0x0f, 0xf7, 0x7f,
    0xf7, 0xff, 0xf7, 0x7f, 0xf7, 0xff, 0xe3, 0x7f, 0xf7, 0x07, 0xe8, 0x7f,
    0xef, 0xf8, 0x67, 0x70, 0x0f, 0xff, 0xbb, 0x6f, 0xf1, 0x00, 0xd0, 0x5b,
    0xfd, 0x3f, 0xec, 0x53, 0xc1, 0xff, 0xef, 0x57, 0x9f, 0xfd, 0xee, 0x5f,
    0x9f, 0xfc, 0xae, 0x5f, 0x1f, 0x78, 0xac, 0x5f, 0x3f, 0x00, 0x50, 0x6c,
    0x7f, 0x00, 0xdc, 0x77, 0xff, 0xc0, 0x3f, 0x78, 0xff, 0x01, 0xf8, 0x7f,
    0xff, 0x03, 0x9c, 0x78, 0xff, 0x07, 0x8c, 0x7c, 0xff, 0x0f, 0xce, 0x78,
    0xff, 0xff, 0xcf, 0x7f, 0xff, 0xff, 0xcf, 0x78, 0xff, 0xff, 0xdf, 0x78,
    0xff, 0xff, 0xdf, 0x7d, 0xff, 0xff, 0x3f, 0x7e, 0xff, 0xff, 0xff, 0x7f,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    Typewriter,
    Scanline,
    Rain,
    Fade,
    Crt,
    Glitch,
    Dissolve,
}

const EFFECTS: [Effect; 7] = [
    Effect::Typewriter,
    Effect::Scanline,
    Effect::Rain,
    Effect::Fade,
    Effect::Crt,
    Effect::Glitch,
    Effect::Dissolve,
];

struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            0
        } else {
            (self.next_u64() % bound as u64) as usize
        }
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next_u64() % 100 < percent
    }
}

fn get_pixel(x: usize, y: usize) -> bool {
    if y >= HEIGHT {
        return false;
    }
    let byte_index = y * BYTES_PER_ROW + x / 8;
    let bit_index = x % 8;
    ((BITS[byte_index] >> bit_index) & 1) == 0
}

fn get_char(x: usize, row: usize) -> char {
    let upper = get_pixel(x, row * 2);
    let lower = get_pixel(x, row * 2 + 1);
    match (upper, lower) {
        (true, true) => '█',
        (true, false) => '▀',
        (false, true) => '▄',
        (false, false) => ' ',
    }
}

fn build_final_grid() -> Vec<Vec<char>> {
    let mut grid = Vec::with_capacity(DISPLAY_HEIGHT);
    for row in 0..DISPLAY_HEIGHT {
        let mut line = Vec::with_capacity(WIDTH);
        for x in 0..WIDTH {
            line.push(get_char(x, row));
        }
        grid.push(line);
    }
    grid
}

fn empty_grid() -> Vec<Vec<char>> {
    vec![vec![' '; WIDTH]; DISPLAY_HEIGHT]
}

fn shuffled_positions(rng: &mut Rng) -> Vec<(usize, usize)> {
    let mut positions: Vec<(usize, usize)> = Vec::with_capacity(DISPLAY_HEIGHT * WIDTH);
    for row in 0..DISPLAY_HEIGHT {
        for x in 0..WIDTH {
            positions.push((row, x));
        }
    }
    for i in (1..positions.len()).rev() {
        let j = rng.below(i + 1);
        positions.swap(i, j);
    }
    positions
}

enum EffectState {
    Typewriter { pos: usize },
    Scanline { row: usize },
    Rain { drops: Vec<Drop> },
    Fade { positions: Vec<(usize, usize)>, index: usize },
    Crt { expansion: usize },
    Glitch { phase: usize, glitch_frames: usize },
    Dissolve { positions: Vec<(usize, usize)>, index: usize },
}

struct Drop {
    y: isize,
    settled: usize,
}

pub struct ArminComponent {
    effect: Effect,
    final_grid: Vec<Vec<char>>,
    current_grid: Vec<Vec<char>>,
    state: EffectState,
    rng: Rng,
    theme: Theme,
}

impl ArminComponent {
    pub fn new(effect: Effect, seed: u64, theme: Theme) -> Self {
        let mut rng = Rng(seed | 1);
        let final_grid = build_final_grid();
        let state = init_effect(effect, &mut rng);
        let current_grid = match effect {
            Effect::Dissolve => random_noise(&mut rng),
            _ => empty_grid(),
        };
        Self {
            effect,
            final_grid,
            current_grid,
            state,
            rng,
            theme,
        }
    }

    pub fn effect(&self) -> Effect {
        self.effect
    }

    /// Advances the animation by one frame; `false` once it is complete.
    pub fn tick(&mut self) -> bool {
        let done = match self.effect {
            Effect::Typewriter => self.tick_typewriter(),
            Effect::Scanline => self.tick_scanline(),
            Effect::Rain => self.tick_rain(),
            Effect::Fade => self.tick_fade(),
            Effect::Crt => self.tick_crt(),
            Effect::Glitch => self.tick_glitch(),
            Effect::Dissolve => self.tick_dissolve(),
        };
        !done
    }

    fn tick_typewriter(&mut self) -> bool {
        let EffectState::Typewriter { pos } = &mut self.state else {
            return true;
        };
        for _ in 0..3 {
            let row = *pos / WIDTH;
            let x = *pos % WIDTH;
            if row >= DISPLAY_HEIGHT {
                return true;
            }
            self.current_grid[row][x] = self.final_grid[row][x];
            *pos += 1;
        }
        false
    }

    fn tick_scanline(&mut self) -> bool {
        let EffectState::Scanline { row } = &mut self.state else {
            return true;
        };
        if *row >= DISPLAY_HEIGHT {
            return true;
        }
        for x in 0..WIDTH {
            self.current_grid[*row][x] = self.final_grid[*row][x];
        }
        *row += 1;
        false
    }

    fn tick_rain(&mut self) -> bool {
        let mut all_settled = true;
        self.current_grid = empty_grid();
        let mut updates: Vec<(usize, usize, isize, bool)> = Vec::with_capacity(WIDTH);
        {
            let EffectState::Rain { drops } = &mut self.state else {
                return true;
            };
            for x in 0..WIDTH {
                let settled = drops[x].settled;
                for row in (DISPLAY_HEIGHT.saturating_sub(settled)..DISPLAY_HEIGHT).rev() {
                    self.current_grid[row][x] = self.final_grid[row][x];
                }
                if settled >= DISPLAY_HEIGHT {
                    updates.push((x, 0, drops[x].y, true));
                    continue;
                }
                all_settled = false;
                let mut target_row: Option<usize> = None;
                for row in (0..DISPLAY_HEIGHT.saturating_sub(settled)).rev() {
                    if self.final_grid[row][x] != ' ' {
                        target_row = Some(row);
                        break;
                    }
                }
                drops[x].y += 1;
                let y = drops[x].y;
                let mut settled_row = None;
                if y >= 0 && (y as usize) < DISPLAY_HEIGHT {
                    match target_row {
                        Some(target) if y as usize >= target => {
                            drops[x].settled = DISPLAY_HEIGHT - target;
                            drops[x].y = -(self.rng.below(5) as isize) - 1;
                        }
                        _ => settled_row = Some(y as usize),
                    }
                }
                updates.push((x, settled_row.unwrap_or(usize::MAX), y, false));
            }
        }
        for (x, row, _y, is_settled) in updates {
            if !is_settled && row != usize::MAX {
                self.current_grid[row][x] = '▓';
            }
        }
        all_settled
    }

    fn tick_fade(&mut self) -> bool {
        let EffectState::Fade { positions, index } = &mut self.state else {
            return true;
        };
        for _ in 0..15 {
            if *index >= positions.len() {
                return true;
            }
            let (row, x) = positions[*index];
            self.current_grid[row][x] = self.final_grid[row][x];
            *index += 1;
        }
        false
    }

    fn tick_crt(&mut self) -> bool {
        let EffectState::Crt { expansion } = &mut self.state else {
            return true;
        };
        let mid_row = DISPLAY_HEIGHT / 2;
        self.current_grid = empty_grid();
        let top = mid_row as isize - *expansion as isize;
        let bottom = mid_row + *expansion;
        for row in top.max(0) as usize..=bottom.min(DISPLAY_HEIGHT - 1) {
            for x in 0..WIDTH {
                self.current_grid[row][x] = self.final_grid[row][x];
            }
        }
        *expansion += 1;
        *expansion > DISPLAY_HEIGHT
    }

    fn tick_glitch(&mut self) -> bool {
        let EffectState::Glitch { phase, glitch_frames } = &mut self.state else {
            return true;
        };
        if *phase < *glitch_frames {
            let mut next = Vec::with_capacity(DISPLAY_HEIGHT);
            for row in &self.final_grid {
                let offset = self.rng.below(7) as isize - 3;
                let mut glitch_row = row.clone();
                if self.rng.chance(30) {
                    let len = glitch_row.len() as isize;
                    let mut shifted = Vec::with_capacity(WIDTH);
                    for i in 0..len {
                        let source = (i + offset).rem_euclid(len);
                        shifted.push(glitch_row[source as usize]);
                    }
                    shifted.truncate(WIDTH);
                    next.push(shifted);
                    continue;
                }
                if self.rng.chance(20) {
                    let swap_row = self.rng.below(DISPLAY_HEIGHT);
                    next.push(self.final_grid[swap_row].clone());
                    continue;
                }
                next.push(std::mem::take(&mut glitch_row));
            }
            self.current_grid = next;
            *phase += 1;
            return false;
        }
        self.current_grid = self.final_grid.clone();
        true
    }

    fn tick_dissolve(&mut self) -> bool {
        let EffectState::Dissolve { positions, index } = &mut self.state else {
            return true;
        };
        for _ in 0..20 {
            if *index >= positions.len() {
                return true;
            }
            let (row, x) = positions[*index];
            self.current_grid[row][x] = self.final_grid[row][x];
            *index += 1;
        }
        false
    }
}

fn init_effect(effect: Effect, rng: &mut Rng) -> EffectState {
    match effect {
        Effect::Typewriter => EffectState::Typewriter { pos: 0 },
        Effect::Scanline => EffectState::Scanline { row: 0 },
        Effect::Rain => EffectState::Rain {
            drops: (0..WIDTH)
                .map(|_| Drop {
                    y: -(rng.below(DISPLAY_HEIGHT * 2) as isize),
                    settled: 0,
                })
                .collect(),
        },
        Effect::Fade => EffectState::Fade {
            positions: shuffled_positions(rng),
            index: 0,
        },
        Effect::Crt => EffectState::Crt { expansion: 0 },
        Effect::Glitch => EffectState::Glitch {
            phase: 0,
            glitch_frames: 8,
        },
        Effect::Dissolve => EffectState::Dissolve {
            positions: shuffled_positions(rng),
            index: 0,
        },
    }
}

fn random_noise(rng: &mut Rng) -> Vec<Vec<char>> {
    let chars = [' ', '░', '▒', '▓', '█', '▀', '▄'];
    (0..DISPLAY_HEIGHT)
        .map(|_| (0..WIDTH).map(|_| chars[rng.below(chars.len())]).collect())
        .collect()
}

impl Component for ArminComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        let padding = 1;
        let available_width = width.saturating_sub(padding);
        let accent = self.theme.fg(ThemeColor::Accent, "");
        let _ = accent;
        let mut lines: Vec<String> = self
            .current_grid
            .iter()
            .map(|row| {
                let clipped: String = row.iter().take(available_width).collect();
                let clipped_len = clipped.chars().count();
                let pad_right = width.saturating_sub(padding + clipped_len);
                format!(" {}{}", self.theme.fg(ThemeColor::Accent, &clipped), " ".repeat(pad_right))
            })
            .collect();
        let message = "ARMIN SAYS HI";
        let msg_pad_right = width.saturating_sub(padding + message.chars().count());
        lines.push(format!(
            " {}{}",
            self.theme.fg(ThemeColor::Accent, message),
            " ".repeat(msg_pad_right)
        ));
        lines
    }

    fn invalidate(&mut self) {}
}

pub fn effect_from_seed(seed: u64) -> Effect {
    EFFECTS[(seed % EFFECTS.len() as u64) as usize]
}
