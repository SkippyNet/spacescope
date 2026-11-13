//! In-process UI test driver.
//!
//! `--script "click:0.3,0.5;key:alt+ArrowUp;wait:0.5;shot:out.png"` replays
//! synthetic input through egui and saves screenshots, so interactions can be
//! verified without touching the real mouse. Coordinates are window fractions
//! when both are <= 1, otherwise points.

use crate::app::Args;
use egui::{pos2, Context, Event, Key, Modifiers, PointerButton, Pos2, RawInput};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

enum Step {
    Move(f32, f32),
    Click(f32, f32, PointerButton, u8),
    Key(Key, Modifiers),
    Wheel(f32),
    Type(String),
    Wait(f32),
    Shot(PathBuf),
    /// Screenshot immediately, even mid-scan or mid-animation.
    ShotNow(PathBuf),
}

pub struct Script {
    steps: VecDeque<Step>,
    frames: VecDeque<(Vec<Event>, Modifiers)>,
    wait_until: Option<Instant>,
    shot: Option<PathBuf>,
    calm_since: Option<Instant>,
}

fn xy(v: &str) -> Result<(f32, f32), String> {
    let (x, y) = v.split_once(',').ok_or(format!("expected x,y in '{v}'"))?;
    Ok((
        x.trim().parse().map_err(|_| format!("bad number '{x}'"))?,
        y.trim().parse().map_err(|_| format!("bad number '{y}'"))?,
    ))
}

impl Script {
    pub fn from_args(args: &Args) -> Result<Option<Script>, String> {
        let mut steps = VecDeque::new();
        if args.script.is_some() || args.screenshot.is_some() {
            steps.push_back(Step::Wait(args.shot_after));
        }
        for part in args.script.as_deref().unwrap_or("").split(';') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let (cmd, val) = part.split_once(':').unwrap_or((part, ""));
            let step = match cmd {
                "move" => xy(val).map(|(x, y)| Step::Move(x, y))?,
                "click" => xy(val).map(|(x, y)| Step::Click(x, y, PointerButton::Primary, 1))?,
                "dbl" => xy(val).map(|(x, y)| Step::Click(x, y, PointerButton::Primary, 2))?,
                "rclick" => xy(val).map(|(x, y)| Step::Click(x, y, PointerButton::Secondary, 1))?,
                "wheel" => Step::Wheel(val.parse().map_err(|_| "bad wheel amount")?),
                "wait" => Step::Wait(val.parse().map_err(|_| "bad wait")?),
                "type" => Step::Type(val.to_string()),
                "shot" => Step::Shot(PathBuf::from(val)),
                "shotnow" => Step::ShotNow(PathBuf::from(val)),
                "key" => {
                    let mut mods = Modifiers::NONE;
                    let mut name = val;
                    while let Some((m, rest)) = name.split_once('+') {
                        match m.to_lowercase().as_str() {
                            "alt" => mods.alt = true,
                            "ctrl" => {
                                mods.ctrl = true;
                                mods.command = true;
                            }
                            "shift" => mods.shift = true,
                            other => return Err(format!("unknown modifier {other}")),
                        }
                        name = rest;
                    }
                    Step::Key(
                        Key::from_name(name).ok_or(format!("unknown key {name}"))?,
                        mods,
                    )
                }
                other => return Err(format!("unknown script step '{other}'")),
            };
            steps.push_back(step);
        }
        if let Some(path) = &args.screenshot {
            if let Some((x, y)) = args.debug_hover {
                steps.push_back(Step::Move(x, y));
            }
            steps.push_back(Step::Shot(path.clone()));
        }
        if steps.is_empty() {
            return Ok(None);
        }
        Ok(Some(Script {
            steps,
            frames: VecDeque::new(),
            wait_until: None,
            shot: None,
            calm_since: None,
        }))
    }

    /// Called from `raw_input_hook`: replaces real pointer input with ours.
    pub fn feed(&mut self, raw: &mut RawInput) {
        raw.events.retain(|e| {
            !matches!(
                e,
                Event::PointerMoved(_)
                    | Event::MouseMoved(_)
                    | Event::PointerButton { .. }
                    | Event::PointerGone
                    | Event::MouseWheel { .. }
            )
        });
        if let Some((events, mods)) = self.frames.pop_front() {
            raw.modifiers = mods;
            raw.events.extend(events);
        }
    }

    /// Advances the script. `settled` means the UI has nothing in flight.
    pub fn drive(&mut self, ctx: &Context, settled: bool) {
        ctx.request_repaint_after(Duration::from_millis(30));
        let shot = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let (Some(img), Some(path)) = (shot, self.shot.clone()) {
            self.shot = None;
            let [w, h] = img.size;
            if let Err(e) = image::save_buffer(
                &path,
                img.as_raw(),
                w as u32,
                h as u32,
                image::ColorType::Rgba8,
            ) {
                eprintln!("screenshot failed: {e}");
            }
        }
        if self.shot.is_some() || !self.frames.is_empty() {
            return;
        }
        if let Some(t) = self.wait_until {
            if Instant::now() < t {
                return;
            }
            self.wait_until = None;
        }
        // Wait for animations and fade-ins to finish between steps, except
        // for steps meant to observe the UI mid-flight.
        let eager = matches!(self.steps.front(), Some(Step::Wait(_) | Step::ShotNow(_)));
        if !eager {
            if !settled {
                self.calm_since = None;
                return;
            }
            let since = *self.calm_since.get_or_insert_with(Instant::now);
            if since.elapsed() < Duration::from_millis(450) {
                return;
            }
        }
        let Some(step) = self.steps.pop_front() else {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        };
        self.calm_since = None;
        let screen = ctx.screen_rect();
        let at = |x: f32, y: f32| -> Pos2 {
            if x <= 1.0 && y <= 1.0 {
                pos2(screen.width() * x, screen.height() * y)
            } else {
                pos2(x, y)
            }
        };
        let none = Modifiers::NONE;
        match step {
            Step::Move(x, y) => self
                .frames
                .push_back((vec![Event::PointerMoved(at(x, y))], none)),
            Step::Click(x, y, button, times) => {
                let pos = at(x, y);
                self.frames
                    .push_back((vec![Event::PointerMoved(pos)], none));
                self.frames.push_back((vec![], none));
                for _ in 0..times {
                    for pressed in [true, false] {
                        self.frames.push_back((
                            vec![Event::PointerButton {
                                pos,
                                button,
                                pressed,
                                modifiers: none,
                            }],
                            none,
                        ));
                    }
                }
            }
            Step::Key(key, modifiers) => {
                for pressed in [true, false] {
                    self.frames.push_back((
                        vec![Event::Key {
                            key,
                            physical_key: None,
                            pressed,
                            repeat: false,
                            modifiers,
                        }],
                        modifiers,
                    ));
                }
            }
            Step::Wheel(dy) => self.frames.push_back((
                vec![Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, dy),
                    modifiers: none,
                }],
                none,
            )),
            Step::Type(text) => self.frames.push_back((vec![Event::Text(text)], none)),
            Step::Wait(secs) => {
                self.wait_until = Some(Instant::now() + Duration::from_secs_f32(secs.max(0.0)))
            }
            Step::Shot(path) | Step::ShotNow(path) => {
                self.shot = Some(path);
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            }
        }
    }
}
