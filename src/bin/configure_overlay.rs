use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
};

use pc_overlay_core::{
    BoardCalibration, FrameSize, WindowLocator, calibration::select_board, capture::capture_client,
};

#[derive(Debug)]
struct Args {
    output: PathBuf,
    mouse: bool,
    cell_px: Option<u32>,
    board_width_px: Option<u32>,
    top_px: Option<u32>,
    center_x_px: Option<u32>,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            output: PathBuf::from("data/overlay-calibration.json"),
            mouse: false,
            cell_px: None,
            board_width_px: None,
            top_px: None,
            center_x_px: None,
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = parse_args()?;
    let Some(window) = WindowLocator::default().locate()? else {
        return Err(
            "visible TETR.IO window was not found; show the game before running configure-overlay"
                .into(),
        );
    };
    let frame = capture_client(&window)?;
    let frame_size = FrameSize {
        width: frame.width,
        height: frame.height,
    };
    let interactive = args.mouse
        || (args.cell_px.is_none()
            && args.board_width_px.is_none()
            && args.top_px.is_none()
            && args.center_x_px.is_none());
    let calibration = if interactive {
        let selection = select_board(window.client_rect)?;
        let center_x_px = selection.left + selection.width / 2;
        if selection.height.abs_diff(selection.width.saturating_mul(2)) > selection.width.max(8) / 4
        {
            eprintln!(
                "warning: selected height {} differs from the 10x20 expectation {}; only the selected width and top edge are used",
                selection.height,
                selection.width.saturating_mul(2)
            );
        }
        BoardCalibration::from_pixels(frame_size, center_x_px, selection.top, selection.width)?
    } else {
        configure_from_numeric(&args, frame_size)?
    };
    write_calibration(&args.output, frame_size, &calibration)?;
    Ok(())
}

fn configure_from_numeric(
    args: &Args,
    frame_size: FrameSize,
) -> Result<BoardCalibration, Box<dyn Error>> {
    let board_width_px = match (args.cell_px, args.board_width_px) {
        (Some(cell_px), None) => cell_px.checked_mul(10).ok_or("--cell-px is too large")?,
        (None, Some(width)) => width,
        (Some(cell_px), Some(width)) => {
            let expected = cell_px.checked_mul(10).ok_or("--cell-px is too large")?;
            if expected != width {
                return Err(format!(
                    "--cell-px {cell_px} implies board width {expected}, but --board-width-px is {width}"
                )
                .into());
            }
            width
        }
        (None, None) => {
            return Err("provide --cell-px or --board-width-px".into());
        }
    };
    let center_x_px = args.center_x_px.unwrap_or(frame_size.width / 2);
    let top_px = args.top_px.ok_or("--top-px is required")?;
    Ok(BoardCalibration::from_pixels(
        frame_size,
        center_x_px,
        top_px,
        board_width_px,
    )?)
}

fn write_calibration(
    output: &Path,
    frame_size: FrameSize,
    calibration: &BoardCalibration,
) -> Result<(), Box<dyn Error>> {
    let json = serde_json::to_string_pretty(&calibration)?;
    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    fs::write(output, format!("{json}\n"))?;

    let rect = calibration.resolve(frame_size);
    println!(
        "saved {}\nclient={}x{} board=({},{} {}x{}) cell_px={:.2}",
        output.display(),
        frame_size.width,
        frame_size.height,
        rect.left,
        rect.top,
        rect.width,
        rect.height,
        rect.width as f32 / 10.0,
    );
    Ok(())
}

fn parse_args() -> Result<Args, Box<dyn Error>> {
    let mut args = Args::default();
    let mut values = std::env::args().skip(1);
    while let Some(arg) = values.next() {
        match arg.as_str() {
            "--output" => {
                args.output = PathBuf::from(values.next().ok_or("--output requires a path")?);
            }
            "--mouse" => args.mouse = true,
            "--cell-px" => {
                args.cell_px = Some(values.next().ok_or("--cell-px requires a value")?.parse()?);
            }
            "--board-width-px" => {
                args.board_width_px = Some(
                    values
                        .next()
                        .ok_or("--board-width-px requires a value")?
                        .parse()?,
                );
            }
            "--top-px" => {
                args.top_px = Some(values.next().ok_or("--top-px requires a value")?.parse()?);
            }
            "--center-x-px" => {
                args.center_x_px = Some(
                    values
                        .next()
                        .ok_or("--center-x-px requires a value")?
                        .parse()?,
                );
            }
            "--help" | "-h" => {
                println!(
                    "configure-overlay [--mouse] [--output PATH]\n\
                     configure-overlay --cell-px N --top-px N [--center-x-px N] [--output PATH]\n\
                     configure-overlay --board-width-px N --top-px N [--center-x-px N] [--output PATH]\n\
                     Without numeric options, starts a mouse drag calibration over the visible board."
                );
                std::process::exit(0);
            }
            unknown => return Err(format!("unknown argument: {unknown}").into()),
        }
    }
    Ok(args)
}
