use std::path::PathBuf;

use pc_overlay_core::{WindowLocator, capture::capture_client};

fn main() {
    let mut bmp_output: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(argument) = args.next() {
        if argument == "--bmp" {
            bmp_output = args.next().map(PathBuf::from);
        }
    }
    match WindowLocator::default().locate() {
        Ok(Some(window)) => match capture_client(&window) {
            Ok(frame) => {
                println!(
                    "captured: {}x{}, bytes={}, well_formed={}, timestamp_ms={}",
                    frame.width,
                    frame.height,
                    frame.rgba.len(),
                    frame.is_well_formed(),
                    frame.captured_at_ms
                );
                if let Some(path) = bmp_output {
                    match frame.write_bmp(path) {
                        Ok(()) => println!("bmp_written"),
                        Err(error) => eprintln!("bmp write failed: {error}"),
                    }
                }
            }
            Err(error) => eprintln!("capture failed: {error}"),
        },
        Ok(None) => println!("not found: no qualifying visible TETR.IO window"),
        Err(error) => eprintln!("window discovery failed: {error}"),
    }
}
