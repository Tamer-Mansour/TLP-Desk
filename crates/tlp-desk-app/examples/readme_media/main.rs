//! README media generator: renders the real TLP-Desk UI headlessly (egui_kittest + wgpu, no
//! window, no network, no profile files) into the screenshots under `docs/media/` and the
//! frames of the demo video.
//!
//! Normally run through `scripts/update-readme-media.ps1`, which also encodes the video and
//! GIF with ffmpeg. Directly:
//!
//! ```text
//! cargo run --release -p tlp-desk-app --example readme_media -- [options]
//!   --out <dir>        screenshots go here (default: <repo>/docs/media)
//!   --frames <dir>     also render the demo; frames go to <dir>/<lang>/f00000.png ...
//!   --only <scene>     render just this scene (repeatable)
//!   --no-stills        skip the screenshots (with --frames: demo only)
//!   --list             list the scenes and exit
//! ```
//!
//! Output is deterministic: same UI code, same bytes. Scenes live in `scenes.rs`; add one
//! entry there to add a screenshot.
#![forbid(unsafe_code)]

mod backend;
mod demo;
mod render;
mod scenes;

use std::path::PathBuf;

use anyhow::{bail, Context as _};
use tlp_desk_app::i18n::Lang;

use render::Look;

/// Languages every scene is rendered in.
const LANGS: [(Lang, &str); 2] = [(Lang::En, "en"), (Lang::Ar, "ar")];

/// The hero image is also rendered at 2x for sharp display on HiDPI screens.
const HIDPI_SCENES: &[&str] = &["home"];

struct Args {
    out: PathBuf,
    frames: Option<PathBuf>,
    only: Vec<String>,
    stills: bool,
    list: bool,
}

fn parse_args() -> anyhow::Result<Args> {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut a = Args {
        out: repo.join("docs").join("media"),
        frames: None,
        only: Vec::new(),
        stills: true,
        list: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || it.next().with_context(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--out" => a.out = PathBuf::from(value()?),
            "--frames" => a.frames = Some(PathBuf::from(value()?)),
            "--only" => a.only.push(value()?),
            "--no-stills" => a.stills = false,
            "--list" => a.list = true,
            other => bail!("unknown argument {other:?} (see the doc comment in main.rs)"),
        }
    }
    for name in &a.only {
        if scenes::find(name).is_none() {
            bail!("unknown scene {name:?}; --list shows them");
        }
    }
    Ok(a)
}

/// Render one scene in one look and write it. Returns (file name, size, changed).
fn still(
    scene: &scenes::Scene,
    look: Look,
    lang_code: &str,
    out: &std::path::Path,
) -> anyhow::Result<(String, usize, bool)> {
    let mut h = render::harness(look);
    (scene.setup)(h.state_mut(), look.lang);
    // A few frames: layout settles (sizing passes, fonts, the texture upload, modals).
    h.run_steps(4);
    let img = render::capture(&mut h)?;
    let png = render::encode_png(&img, true)?;
    let suffix = if look.pixels_per_point > 1.0 {
        "@2x"
    } else {
        ""
    };
    let name = format!(
        "{}-{lang_code}-{}{suffix}.png",
        scene.name,
        look.theme_name()
    );
    let changed = render::write_if_changed(&out.join(&name), &png)?;
    Ok((name, png.len(), changed))
}

fn main() -> anyhow::Result<()> {
    let args = parse_args()?;
    if args.list {
        for s in scenes::SCENES {
            println!("{:<14} {}", s.name, s.about);
        }
        return Ok(());
    }

    if args.stills {
        let selected = scenes::SCENES
            .iter()
            .filter(|s| args.only.is_empty() || args.only.iter().any(|o| o == s.name));
        for scene in selected {
            for (lang, code) in LANGS {
                for dark in [true, false] {
                    let mut scales = vec![1.0];
                    if HIDPI_SCENES.contains(&scene.name) {
                        scales.push(2.0);
                    }
                    for pixels_per_point in scales {
                        let look = Look {
                            lang,
                            dark,
                            pixels_per_point,
                            step_dt: 0.25,
                        };
                        let (name, size, changed) = still(scene, look, code, &args.out)?;
                        let mark = if changed { "updated" } else { "same" };
                        println!("{name:<34} {:>8} KB  {mark}", size.div_ceil(1024));
                    }
                }
            }
        }
    }

    if let Some(frames) = &args.frames {
        for (lang, code) in LANGS {
            let dir = frames.join(code);
            let n = demo::render(lang, &dir)?;
            println!("demo-{code}: {n} frames in {}", dir.display());
        }
    }
    Ok(())
}
