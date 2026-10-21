use std::{error::Error, path::PathBuf};

use pc_overlay_core::{KonbiniLibrary, konbini};

/// Downloads the Setup Konbini data sheets into the cache folder that
/// `run-overlay` reads (default `data/konbini`), then reports what was read.
fn main() -> Result<(), Box<dyn Error>> {
    let mut dir = PathBuf::from("data/konbini");
    let mut values = std::env::args().skip(1);
    while let Some(arg) = values.next() {
        match arg.as_str() {
            "--dir" => dir = PathBuf::from(values.next().ok_or("--dir requires a folder")?),
            "--help" | "-h" => {
                println!("fetch-konbini [--dir DIR]");
                return Ok(());
            }
            unknown => return Err(format!("unknown argument: {unknown}").into()),
        }
    }
    konbini::fetch_all(&dir)?;
    let library = KonbiniLibrary::load_dir(&dir)?;
    for stage in 1..=konbini::STAGE_SHEETS.len() as u8 {
        println!(
            "{}: {} setups",
            konbini::stage_label(stage),
            library.stage_count(stage)
        );
    }
    for issue in &library.issues {
        eprintln!("warning: {issue}");
    }
    println!("saved to {}", dir.display());
    Ok(())
}
