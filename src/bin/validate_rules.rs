use std::{env, process::ExitCode};

use pc_overlay_core::rules::{ReviewStatus, RuleSet};

fn main() -> ExitCode {
    let path = env::args()
        .nth(1)
        .unwrap_or_else(|| "data/ijlo.json".to_owned());
    match RuleSet::load(&path) {
        Ok(rules) => {
            let methods = rules.methods.len();
            let setups: Vec<_> = rules
                .methods
                .iter()
                .flat_map(|method| &method.setups)
                .collect();
            let verified = setups
                .iter()
                .filter(|setup| setup.review_status == ReviewStatus::Verified)
                .count();
            println!(
                "valid: case={}, methods={}, setups={}, verified={}, review_required={}",
                rules.case.id,
                methods,
                setups.len(),
                verified,
                setups.len() - verified
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
