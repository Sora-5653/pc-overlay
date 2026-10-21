use std::{error::Error, fs, path::PathBuf};

use pc_overlay_core::{
    BoardCalibration, CapturedFrame, RecognizerConfig, TetrioRecognizer, piece_code,
};

#[derive(Debug)]
struct Args {
    bmp: PathBuf,
    calibration: PathBuf,
    cells: bool,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            bmp: PathBuf::new(),
            calibration: PathBuf::from("data/overlay-calibration.json"),
            cells: false,
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = parse_args()?;
    if args.bmp.as_os_str().is_empty() {
        return Err("--bmp is required".into());
    }
    let frame = read_bmp(&args.bmp)?;
    let calibration = if args.calibration.exists() {
        let text = fs::read_to_string(&args.calibration)?;
        let calibration: BoardCalibration = serde_json::from_str(&text)?;
        calibration.validate()?;
        calibration
    } else {
        BoardCalibration::default()
    };
    let recognizer = TetrioRecognizer::new(RecognizerConfig {
        board_calibration: calibration,
        ..Default::default()
    })?;
    let result = recognizer.recognize(&frame)?;
    let hold = result.hold.map(piece_code).unwrap_or('-');
    let current = result.current.map(piece_code).unwrap_or('-');
    let next = result
        .next5
        .iter()
        .map(|piece| piece.map(piece_code).unwrap_or('-'))
        .collect::<String>();
    println!(
        "frame={}x{} board=({},{} {}x{}) current={} hold={} next={} confidence={:.3} observation={}",
        frame.width,
        frame.height,
        result.board_rect.left,
        result.board_rect.top,
        result.board_rect.width,
        result.board_rect.height,
        current,
        hold,
        next,
        result.confidence,
        result.observation.is_some(),
    );
    println!(
        "region_confidence current={:.3} hold={:.3} next={:?} board_cells={} weak={} ambiguous={}",
        result.current_confidence,
        result.hold_confidence,
        result
            .next_confidences
            .iter()
            .map(|confidence| format!("{confidence:.3}"))
            .collect::<Vec<_>>(),
        result.board_cells,
        result.weak_board_cells,
        result.ambiguous_board_cells,
    );
    if args.cells {
        // One line per board row, top first: piece letter plus classified
        // pixel count for every cell, or `.` when nothing was classified.
        for row in &result.cell_samples {
            let line = row
                .iter()
                .map(|cell| match cell {
                    Some((piece, hits, _)) => format!("{}{:>3}", piece_code(*piece), hits),
                    None => "   .".to_string(),
                })
                .collect::<Vec<_>>()
                .join(" ");
            println!("{line}");
        }
    }
    Ok(())
}

fn parse_args() -> Result<Args, Box<dyn Error>> {
    let mut args = Args::default();
    let mut values = std::env::args().skip(1);
    while let Some(arg) = values.next() {
        match arg.as_str() {
            "--bmp" => args.bmp = PathBuf::from(values.next().ok_or("--bmp requires a path")?),
            "--calibration" => {
                args.calibration =
                    PathBuf::from(values.next().ok_or("--calibration requires a path")?);
            }
            "--cells" => args.cells = true,
            "--help" | "-h" => {
                println!("probe-recognition --bmp PATH [--calibration PATH] [--cells]");
                std::process::exit(0);
            }
            unknown => return Err(format!("unknown argument: {unknown}").into()),
        }
    }
    Ok(args)
}

fn read_bmp(path: &PathBuf) -> Result<CapturedFrame, Box<dyn Error>> {
    let bytes = fs::read(path)?;
    if bytes.len() < 54 || &bytes[0..2] != b"BM" {
        return Err("unsupported BMP header".into());
    }
    let pixel_offset = u32::from_le_bytes(bytes[10..14].try_into()?) as usize;
    let dib_size = u32::from_le_bytes(bytes[14..18].try_into()?);
    if dib_size < 40 {
        return Err("unsupported BMP DIB header".into());
    }
    let width = i32::from_le_bytes(bytes[18..22].try_into()?);
    let signed_height = i32::from_le_bytes(bytes[22..26].try_into()?);
    let planes = u16::from_le_bytes(bytes[26..28].try_into()?);
    let bits_per_pixel = u16::from_le_bytes(bytes[28..30].try_into()?);
    let compression = u32::from_le_bytes(bytes[30..34].try_into()?);
    if width <= 0
        || signed_height == 0
        || planes != 1
        || !matches!(bits_per_pixel, 24 | 32)
        || compression != 0
    {
        return Err("only uncompressed 24/32-bit BMP files are supported".into());
    }
    let height = signed_height.unsigned_abs();
    let bytes_per_pixel = usize::from(bits_per_pixel / 8);
    let row_stride = (width as usize * bytes_per_pixel).div_ceil(4) * 4;
    let required = pixel_offset
        .checked_add(
            row_stride
                .checked_mul(height as usize)
                .ok_or("BMP is too large")?,
        )
        .ok_or("BMP is too large")?;
    if required > bytes.len() {
        return Err("BMP pixel data is truncated".into());
    }
    let mut rgba = vec![0u8; width as usize * height as usize * 4];
    for y in 0..height as usize {
        let source_y = if signed_height > 0 {
            height as usize - 1 - y
        } else {
            y
        };
        let source_row = pixel_offset + source_y * row_stride;
        for x in 0..width as usize {
            let source = source_row + x * bytes_per_pixel;
            let destination = (y * width as usize + x) * 4;
            rgba[destination..destination + 4].copy_from_slice(&[
                bytes[source + 2],
                bytes[source + 1],
                bytes[source],
                if bytes_per_pixel == 4 {
                    bytes[source + 3]
                } else {
                    255
                },
            ]);
        }
    }
    Ok(CapturedFrame {
        width: width as u32,
        height,
        rgba,
        captured_at_ms: 0,
    })
}
