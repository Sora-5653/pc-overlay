use std::{env, process::ExitCode};

use pc_overlay_core::diagram::DiagramSet;

fn main() -> ExitCode {
    let path = env::args()
        .nth(1)
        .unwrap_or_else(|| "data/ijlo/diagram_cells.json".to_owned());
    match DiagramSet::load(&path) {
        Ok(diagrams) => match diagrams.validate() {
            Ok(report) => {
                println!(
                    "valid: diagrams={}, placements={}, warnings={}",
                    report.diagram_count,
                    report.placement_count,
                    report.warnings.len()
                );
                for warning in report.warnings {
                    println!("warning: {warning}");
                }
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
