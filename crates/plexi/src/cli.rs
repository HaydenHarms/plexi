//! `plexi export` for scripts and quick conversions without opening a window.

use std::path::PathBuf;
use std::process::ExitCode;

use plexi_core::export::{ExportFormat, RenderOptions, export};
use plexi_core::{Page, format};

const USAGE: &str = "\
Plexi — draw and take notes anywhere, right on top of your screen

Usage:
  plexi [FILE.plexi]           Open the notebook (or the last one)
  plexi --draw                 Start straight in draw-on-screen mode
  plexi --background           Start hidden; press the draw hotkey to begin
  plexi export IN.plexi OUT [--page N | --all | --screen] [--scale S] [--no-background]

OUT's extension picks the format: pdf png jpg webp bmp tiff svg html md txt
excalidraw drawio inkml json. Raster and SVG exports of several pages write
OUT-1.ext, OUT-2.ext, …";

pub fn is_cli(args: &[String]) -> bool {
    matches!(args.first().map(String::as_str), Some("export" | "--help" | "-h" | "help" | "--version" | "-V"))
}

pub fn run(args: &[String]) -> ExitCode {
    match args.first().map(String::as_str) {
        Some("--version" | "-V") => {
            println!("plexi {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("export") => match export_cmd(&args[1..]) {
            Ok(files) => {
                for f in files {
                    println!("{}", f.display());
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("plexi: {e}");
                ExitCode::FAILURE
            }
        },
        _ => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
    }
}

fn export_cmd(args: &[String]) -> Result<Vec<PathBuf>, String> {
    let mut positional = Vec::new();
    let mut page: Option<usize> = None;
    let mut all = false;
    let mut screen = false;
    let mut opts = RenderOptions::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--all" => all = true,
            "--screen" => screen = true,
            "--no-background" => opts.background = false,
            "--page" => {
                let n: usize = it.next().and_then(|v| v.parse().ok()).ok_or("--page needs a number")?;
                page = Some(n.max(1) - 1);
            }
            "--scale" => opts.scale = it.next().and_then(|v| v.parse().ok()).ok_or("--scale needs a number")?,
            s if s.starts_with("--") => return Err(format!("unknown option {s}\n\n{USAGE}")),
            _ => positional.push(PathBuf::from(a)),
        }
    }
    let [input, output] = positional.as_slice() else { return Err(USAGE.into()) };
    let doc = format::load(input).map_err(|e| format!("{}: {e}", input.display()))?;
    let ext = output.extension().and_then(|e| e.to_str()).unwrap_or("");
    let fmt = ExportFormat::from_extension(ext).ok_or_else(|| format!("don't know how to export .{ext}"))?;
    let pages: Vec<&Page> = if screen {
        vec![&doc.overlay]
    } else if all || (page.is_none() && !fmt.one_page_per_file()) {
        doc.pages.iter().collect()
    } else {
        let i = page.unwrap_or(0);
        vec![doc.pages.get(i).ok_or_else(|| format!("there is no page {}", i + 1))?]
    };
    export(&doc, &pages, fmt, &opts, output).map_err(|e| e.to_string())
}
