use std::{
    collections::{BTreeSet, HashSet},
    fs,
    path::Path,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    condition::{Condition, EvalContext, EvalResult},
    diagram::DiagramSet,
    guidance::{SolveProgress, evaluate_setup_progress, evaluate_solve_progress},
    model::{BOARD_WIDTH, Board, Cell, Piece, Placement},
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    Verified,
    #[default]
    ReviewRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MethodCategory {
    /// A generated case's main section (tools/build_cases.py).
    Main,
    SaveO,
    Alt,
    TI,
    Extra,
}

impl MethodCategory {
    const fn rank(self) -> u8 {
        match self {
            Self::Main | Self::SaveO => 0,
            Self::Alt => 1,
            Self::TI => 2,
            Self::Extra => 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceReference {
    pub document: String,
    pub lines: [usize; 2],
    #[serde(default)]
    pub images: Vec<String>,
    #[serde(default)]
    pub note: Option<String>,
}

/// The headings of a source document, to name the section a cited line is
/// in ("Odd JL, with T › TIJZ/TILS"). Rules cite source lines, not headings,
/// so the names follow the document instead of being copied into every rule.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceSections {
    /// `(line, level, title)` for `##` and `###` headings, in order.
    headings: Vec<(usize, usize, String)>,
}

impl SourceSections {
    pub fn load(path: impl AsRef<Path>) -> std::io::Result<Self> {
        Ok(Self::parse(&fs::read_to_string(path)?))
    }

    pub fn parse(markdown: &str) -> Self {
        let mut headings = Vec::new();
        for (index, line) in markdown.lines().enumerate() {
            let level = line.chars().take_while(|ch| *ch == '#').count();
            if !(2..=3).contains(&level) || !line[level..].starts_with(' ') {
                continue;
            }
            // Drop images, Markdown escapes, and the source's `*` markers.
            let mut title = line[level..].to_string();
            while let Some(start) = title.find("![][") {
                let name = start + "![][".len();
                let end = title[name..]
                    .find(']')
                    .map_or(title.len(), |at| name + at + 1);
                title.replace_range(start..end, "");
            }
            let title = title.replace(['\\', '*'], "");
            let title = title.trim();
            // A heading made only of images names no section.
            if !title.is_empty() {
                headings.push((index + 1, level, title.to_string()));
            }
        }
        Self { headings }
    }

    /// The `##` section and, inside it, the `###` subsection that `line`
    /// (1-based) belongs to, joined by ` › `.
    pub fn section(&self, line: usize) -> Option<String> {
        let before = self.headings.partition_point(|(at, _, _)| *at <= line);
        let headings = &self.headings[..before];
        let (major_at, major) = headings
            .iter()
            .rposition(|(_, level, _)| *level == 2)
            .map(|index| (index, headings[index].2.as_str()))?;
        let minor = headings[major_at + 1..]
            .iter()
            .rev()
            .find(|(_, level, _)| *level == 3)
            .map(|(_, _, title)| title.as_str());
        Some(match minor {
            Some(minor) => format!("{major} › {minor}"),
            None => major.to_string(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseDefinition {
    pub id: String,
    pub family: String,
    pub source: SourceReference,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SolveRule {
    pub id: String,
    pub source_order: u32,
    #[serde(default)]
    pub review_status: ReviewStatus,
    pub condition: Condition,
    #[serde(default)]
    pub target: Vec<Placement>,
    pub source: SourceReference,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupRule {
    pub id: String,
    pub source_order: u32,
    /// Optional local priority from the source. Lower values win before global
    /// tie-breaks; absent values retain document order.
    #[serde(default)]
    pub local_priority: Option<u32>,
    pub setup_piece_count: u8,
    #[serde(default)]
    pub review_status: ReviewStatus,
    pub build_condition: Condition,
    #[serde(default)]
    pub fail_condition: Option<Condition>,
    #[serde(default)]
    pub save: Option<Piece>,
    #[serde(default)]
    pub placements: Vec<Placement>,
    #[serde(default)]
    pub solves: Vec<SolveRule>,
    pub source: SourceReference,
}

impl SetupRule {
    pub fn solve_candidates<'a>(
        &'a self,
        context: &EvalContext,
        show_solve: bool,
    ) -> Vec<&'a SolveRule> {
        if !show_solve {
            return Vec::new();
        }
        let mut solves: Vec<_> = self
            .solves
            .iter()
            .filter(|solve| {
                solve.review_status == ReviewStatus::Verified
                    && solve.condition.evaluate(context) == EvalResult::Satisfied
            })
            .collect();
        solves.sort_by_key(|solve| solve.source_order);
        solves
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MethodRule {
    pub id: String,
    pub source_order: u32,
    pub category: MethodCategory,
    pub setups: Vec<SetupRule>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleSet {
    pub schema_version: u32,
    pub case: CaseDefinition,
    pub methods: Vec<MethodRule>,
}

#[derive(Debug, Error)]
pub enum RuleLoadError {
    #[error("could not read rule data: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid rule JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Validation(#[from] RuleValidationError),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("rule validation failed:\n{}", .issues.join("\n"))]
pub struct RuleValidationError {
    pub issues: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateStatus {
    Eligible,
    ReviewRequired,
    BuildPending,
    BuildFailed,
    FailConditionMatched,
    SetupMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateReport {
    pub method_id: String,
    pub setup_id: String,
    pub status: CandidateStatus,
    pub build_result: EvalResult,
    pub fail_result: Option<EvalResult>,
    pub satisfied_at: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    pub case_id: String,
    pub method_id: String,
    pub setup_id: String,
    pub save: Option<Piece>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionReport {
    pub selected: Option<Selection>,
    pub candidates: Vec<CandidateReport>,
}

impl RuleSet {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, RuleLoadError> {
        let contents = fs::read_to_string(path)?;
        let rules: Self = serde_json::from_str(&contents)?;
        rules.validate()?;
        Ok(rules)
    }

    /// Attaches geometry transcribed from source diagrams without changing the
    /// review gate or inventing semantic conditions. Setup source references
    /// contain exactly one image; solve references remain untouched because a
    /// solve may contain multiple line-clear phases.
    pub fn attach_setup_diagrams(&mut self, diagrams: &DiagramSet) -> Result<(), Vec<String>> {
        let mut issues = Vec::new();
        for method in &mut self.methods {
            for setup in &mut method.setups {
                let Some(image) = setup.source.images.first() else {
                    issues.push(format!("setup {} has no source image", setup.id));
                    continue;
                };
                let Some(diagram) = diagrams.diagram(image) else {
                    issues.push(format!(
                        "setup {} references missing diagram {}",
                        setup.id, image
                    ));
                    continue;
                };
                if setup.source.images.len() != 1 {
                    issues.push(format!(
                        "setup {} references multiple diagrams; refusing implicit merge",
                        setup.id
                    ));
                    continue;
                }
                if setup.placements.is_empty() {
                    setup.placements = diagram
                        .placements
                        .iter()
                        .map(|placement| placement.as_placement())
                        .collect();
                }
            }
        }
        if issues.is_empty() {
            Ok(())
        } else {
            Err(issues)
        }
    }

    pub fn validate(&self) -> Result<(), RuleValidationError> {
        let mut issues = Vec::new();
        if self.schema_version != 1 {
            issues.push(format!(
                "schema_version must be 1, got {}",
                self.schema_version
            ));
        }
        if self.case.id.trim().is_empty() {
            issues.push("case.id must not be empty".into());
        }
        validate_source("case.source", &self.case.source, &mut issues);

        let mut method_ids = HashSet::new();
        let mut method_orders = HashSet::new();
        let mut all_setup_ids = HashSet::new();
        for method in &self.methods {
            let method_path = format!("methods[{}]", method.id);
            if !method_ids.insert(method.id.as_str()) {
                issues.push(format!("duplicate method id: {}", method.id));
            }
            if !method_orders.insert(method.source_order) {
                issues.push(format!(
                    "duplicate method source_order: {}",
                    method.source_order
                ));
            }
            if method.setups.is_empty() {
                issues.push(format!("{method_path}.setups must not be empty"));
            }

            let mut setup_orders = HashSet::new();
            for setup in &method.setups {
                let setup_path = format!("{method_path}.setups[{}]", setup.id);
                if !all_setup_ids.insert(setup.id.as_str()) {
                    issues.push(format!("duplicate setup id: {}", setup.id));
                }
                if !setup_orders.insert(setup.source_order) {
                    issues.push(format!(
                        "duplicate setup source_order {} in method {}",
                        setup.source_order, method.id
                    ));
                }
                if setup.setup_piece_count == 0 {
                    issues.push(format!("{setup_path}.setup_piece_count must be positive"));
                }
                validate_source(&format!("{setup_path}.source"), &setup.source, &mut issues);
                validate_placements(
                    &format!("{setup_path}.placements"),
                    &setup.placements,
                    setup.review_status == ReviewStatus::Verified,
                    &mut issues,
                );

                let mut solve_ids = HashSet::new();
                let mut solve_orders = HashSet::new();
                for solve in &setup.solves {
                    let solve_path = format!("{setup_path}.solves[{}]", solve.id);
                    if !solve_ids.insert(solve.id.as_str()) {
                        issues.push(format!("duplicate solve id in {}: {}", setup.id, solve.id));
                    }
                    if !solve_orders.insert(solve.source_order) {
                        issues.push(format!(
                            "duplicate solve source_order {} in setup {}",
                            solve.source_order, setup.id
                        ));
                    }
                    validate_source(&format!("{solve_path}.source"), &solve.source, &mut issues);
                    validate_placements(
                        &format!("{solve_path}.target"),
                        &solve.target,
                        solve.review_status == ReviewStatus::Verified,
                        &mut issues,
                    );
                    if solve.review_status == ReviewStatus::Verified {
                        validate_perfect_clear(
                            &solve_path,
                            &setup.placements,
                            &solve.target,
                            &mut issues,
                        );
                    }
                }
            }
        }

        if issues.is_empty() {
            Ok(())
        } else {
            Err(RuleValidationError { issues })
        }
    }

    /// Selects only human-verified rules. Review-required data is visible in the
    /// report but cannot produce a live recommendation.
    pub fn select(&self, context: &EvalContext) -> SelectionReport {
        self.select_internal(context, None, false)
    }

    /// Selects a candidate while also checking that already-fixed cells are
    /// compatible with its registered setup. Missing expected cells are fine:
    /// they are precisely what the overlay should guide. An unexpected or
    /// wrong-colored fixed cell rejects that candidate and allows a registered
    /// fallback to be considered.
    pub fn select_for_board(&self, context: &EvalContext, board: &Board) -> SelectionReport {
        self.select_internal(context, Some(board), false)
    }

    /// Selects a structurally valid rule even when its source transcription is
    /// still marked as requiring review.  This is intended for an explicitly
    /// labelled learning preview; callers must not present the result as a
    /// verified recommendation.
    pub fn select_preview_for_board(
        &self,
        context: &EvalContext,
        board: &Board,
    ) -> SelectionReport {
        self.select_internal(context, Some(board), true)
    }

    /// Returns the registered setup behind a selection so a rendering surface
    /// can draw only geometry that came from the validated rule data.
    pub fn setup(&self, method_id: &str, setup_id: &str) -> Option<&SetupRule> {
        self.methods
            .iter()
            .find(|method| method.id == method_id)
            .and_then(|method| method.setups.iter().find(|setup| setup.id == setup_id))
    }

    fn select_internal(
        &self,
        context: &EvalContext,
        board: Option<&Board>,
        include_review_required: bool,
    ) -> SelectionReport {
        let mut candidates = Vec::new();
        let mut eligible = Vec::new();
        // Verified setups still undecided, with the share of bag completions
        // they solve; used only when no setup is decided (generated cases).
        let mut likely = Vec::new();

        for method in &self.methods {
            for setup in &method.setups {
                let build_result = setup.build_condition.evaluate(context);
                let fail_result = setup
                    .fail_condition
                    .as_ref()
                    .map(|condition| condition.evaluate(context));
                let satisfied_at = setup.build_condition.satisfied_at(context);
                let setup_mismatch = board.is_some_and(|board| {
                    let progress = evaluate_setup_progress(&setup.placements, board);
                    let deviates = progress.cells.iter().any(|cell| {
                        cell.status == crate::guidance::ExpectedCellStatus::OccupiedByOtherPiece
                    }) || !progress.unexpected_fixed_cells.is_empty();
                    // Solve pieces placed early, or rows already cleared by the
                    // solve, are still this setup as long as a verified solve
                    // explains the board.
                    deviates
                        && !setup.solves.iter().any(|solve| {
                            solve.review_status == ReviewStatus::Verified
                                && matches!(
                                    evaluate_solve_progress(
                                        &setup.placements,
                                        &solve.target,
                                        board
                                    ),
                                    SolveProgress::InProgress { .. }
                                )
                        })
                });
                let status = if setup.review_status != ReviewStatus::Verified {
                    CandidateStatus::ReviewRequired
                } else if build_result == EvalResult::Pending {
                    CandidateStatus::BuildPending
                } else if build_result == EvalResult::Failed {
                    CandidateStatus::BuildFailed
                } else if fail_result == Some(EvalResult::Satisfied) {
                    CandidateStatus::FailConditionMatched
                } else if setup_mismatch {
                    CandidateStatus::SetupMismatch
                } else {
                    CandidateStatus::Eligible
                };
                candidates.push(CandidateReport {
                    method_id: method.id.clone(),
                    setup_id: setup.id.clone(),
                    status,
                    build_result,
                    fail_result,
                    satisfied_at,
                });
                let preview_eligible = include_review_required
                    && setup.review_status == ReviewStatus::ReviewRequired
                    && build_result == EvalResult::Satisfied
                    && fail_result != Some(EvalResult::Satisfied)
                    && !setup_mismatch;
                if status == CandidateStatus::Eligible || preview_eligible {
                    eligible.push((method, setup, satisfied_at.unwrap_or(usize::MAX)));
                }
                if status == CandidateStatus::BuildPending
                    && !setup_mismatch
                    && fail_result != Some(EvalResult::Satisfied)
                    && let Some((works, total)) = setup.build_condition.success_ratio(context)
                    && works > 0
                {
                    likely.push((method, setup, works, total));
                }
            }
        }

        eligible.sort_by_key(|(method, setup, satisfied_at)| {
            (
                method.category.rank(),
                setup.local_priority.unwrap_or(u32::MAX),
                *satisfied_at,
                setup.setup_piece_count,
                method.source_order,
                setup.source_order,
            )
        });
        if eligible.is_empty() && !likely.is_empty() {
            // Highest share first (compared as works_a * total_b), then the
            // usual source ordering.
            likely.sort_by(|a, b| {
                (b.2 * a.3)
                    .cmp(&(a.2 * b.3))
                    .then(a.0.category.rank().cmp(&b.0.category.rank()))
                    .then(a.0.source_order.cmp(&b.0.source_order))
                    .then(a.1.source_order.cmp(&b.1.source_order))
            });
            let (method, setup, works, total) = likely[0];
            return SelectionReport {
                selected: Some(Selection {
                    case_id: self.case.id.clone(),
                    method_id: method.id.clone(),
                    setup_id: setup.id.clone(),
                    save: setup.save,
                    reason: format!(
                        "likely: solves {works} of {total} remaining bag orders ({:.1}%), no setup decided yet",
                        100.0 * works as f64 / total as f64
                    ),
                }),
                candidates,
            };
        }
        let selected = eligible.first().map(|(method, setup, satisfied_at)| Selection {
            case_id: self.case.id.clone(),
            method_id: method.id.clone(),
            setup_id: setup.id.clone(),
            save: setup.save,
            reason: format!(
                "category={:?}, condition_satisfied_at={}, setup_piece_count={}, source_order={}:{}",
                method.category, satisfied_at, setup.setup_piece_count, method.source_order, setup.source_order
            ),
        });

        SelectionReport {
            selected,
            candidates,
        }
    }
}

fn validate_source(path: &str, source: &SourceReference, issues: &mut Vec<String>) {
    if source.document.trim().is_empty() {
        issues.push(format!("{path}.document must not be empty"));
    }
    if source.lines[0] == 0 || source.lines[0] > source.lines[1] {
        issues.push(format!("{path}.lines must be a valid one-based range"));
    }
}

/// A verified solve, together with its setup, must fill every row it touches
/// without overlap; solve progress tracking relies on this to infer clears.
fn validate_perfect_clear(
    path: &str,
    setup: &[Placement],
    target: &[Placement],
    issues: &mut Vec<String>,
) {
    let mut occupied = BTreeSet::<Cell>::new();
    for cell in setup
        .iter()
        .chain(target)
        .flat_map(|placement| &placement.cells)
    {
        if !occupied.insert(*cell) {
            issues.push(format!(
                "{path}.target overlaps the setup or itself at ({},{})",
                cell.x, cell.y
            ));
        }
    }
    let height = occupied.iter().map(|cell| cell.y + 1).max().unwrap_or(0);
    for y in 0..height {
        if (0..BOARD_WIDTH as u8).any(|x| !occupied.contains(&Cell { x, y })) {
            issues.push(format!(
                "{path}.target with its setup leaves row {y} incomplete"
            ));
        }
    }
}

fn validate_placements(
    path: &str,
    placements: &[Placement],
    require_complete: bool,
    issues: &mut Vec<String>,
) {
    if require_complete && placements.is_empty() {
        issues.push(format!("{path} must not be empty for a verified rule"));
    }
    let mut occupied = BTreeSet::<(u8, u8)>::new();
    for (index, placement) in placements.iter().enumerate() {
        if placement.cells.len() != 4 {
            issues.push(format!(
                "{path}[{index}] must contain exactly four cells, got {}",
                placement.cells.len()
            ));
        }
        let unique: BTreeSet<Cell> = placement.cells.iter().copied().collect();
        if unique.len() != placement.cells.len() {
            issues.push(format!("{path}[{index}] contains duplicate cells"));
        }
        for cell in &placement.cells {
            if !cell.in_bounds() {
                issues.push(format!(
                    "{path}[{index}] cell ({},{}) is outside the 10x20 board",
                    cell.x, cell.y
                ));
            }
            if !occupied.insert((cell.x, cell.y)) {
                issues.push(format!(
                    "{path}[{index}] overlaps another placement at ({},{})",
                    cell.x, cell.y
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Cell;

    #[test]
    fn source_sections_name_the_heading_and_subheading_of_a_line() {
        let sections = SourceSections::parse(
            "# **Title**\n\
             ## Odd JL, with T\n\
             intro\n\
             ### TIJS/TILZ \n\
             text\n\
             ### TIJZ/TILS \n\
             setup ![][image434]\n\
             ### ![][image561] ![][image562]\n\
             after images\n\
             ## Other Necessary Dupes\n\
             ### TJOS/TLOZ\\*\n",
        );
        assert_eq!(sections.section(1), None);
        assert_eq!(sections.section(3).as_deref(), Some("Odd JL, with T"));
        assert_eq!(
            sections.section(7).as_deref(),
            Some("Odd JL, with T › TIJZ/TILS")
        );
        // An image-only heading keeps the previous subsection.
        assert_eq!(
            sections.section(9).as_deref(),
            Some("Odd JL, with T › TIJZ/TILS")
        );
        assert_eq!(
            sections.section(11).as_deref(),
            Some("Other Necessary Dupes › TJOS/TLOZ")
        );
    }

    fn source() -> SourceReference {
        SourceReference {
            document: "source.md".into(),
            lines: [1, 2],
            images: Vec::new(),
            note: None,
        }
    }

    fn placement(piece: Piece, x: u8) -> Placement {
        Placement {
            piece,
            cells: vec![
                Cell { x, y: 0 },
                Cell { x, y: 1 },
                Cell { x, y: 2 },
                Cell { x, y: 3 },
            ],
        }
    }

    fn setup(id: &str, source_order: u32, pieces: u8) -> SetupRule {
        SetupRule {
            id: id.into(),
            source_order,
            local_priority: None,
            setup_piece_count: pieces,
            review_status: ReviewStatus::Verified,
            build_condition: Condition::Seen { piece: Piece::S },
            fail_condition: None,
            save: Some(Piece::O),
            placements: vec![placement(Piece::I, source_order as u8)],
            solves: Vec::new(),
            source: source(),
        }
    }

    fn context() -> EvalContext {
        EvalContext {
            queue_history: vec![Piece::S, Piece::T],
            queue_closed: false,
            hold: None,
            current: Piece::I,
            accessible: BTreeSet::new(),
            case_hold: None,
            case_queue: Vec::new(),
        }
    }

    #[test]
    fn category_priority_precedes_setup_size() {
        let rules = RuleSet {
            schema_version: 1,
            case: CaseDefinition {
                id: "IJLO".into(),
                family: "even_jl_no_t".into(),
                source: source(),
            },
            methods: vec![
                MethodRule {
                    id: "alt".into(),
                    source_order: 1,
                    category: MethodCategory::Alt,
                    setups: vec![setup("small_alt", 1, 1)],
                },
                MethodRule {
                    id: "save_o".into(),
                    source_order: 2,
                    category: MethodCategory::SaveO,
                    setups: vec![setup("large_save_o", 1, 4)],
                },
            ],
        };
        rules.validate().unwrap();
        assert_eq!(
            rules.select(&context()).selected.unwrap().setup_id,
            "large_save_o"
        );
    }

    #[test]
    fn fail_condition_excludes_an_otherwise_eligible_setup() {
        let mut failed = setup("failed", 1, 1);
        failed.fail_condition = Some(Condition::Seen { piece: Piece::T });
        let rules = RuleSet {
            schema_version: 1,
            case: CaseDefinition {
                id: "IJLO".into(),
                family: "even_jl_no_t".into(),
                source: source(),
            },
            methods: vec![MethodRule {
                id: "save_o".into(),
                source_order: 1,
                category: MethodCategory::SaveO,
                setups: vec![failed, setup("fallback", 2, 2)],
            }],
        };
        assert_eq!(
            rules.select(&context()).selected.unwrap().setup_id,
            "fallback"
        );
    }

    #[test]
    fn board_mismatch_uses_a_registered_fallback() {
        let first = setup("first", 1, 1);
        let mut fallback = setup("fallback", 2, 1);
        fallback.placements = vec![placement(Piece::O, 1)];
        let rules = RuleSet {
            schema_version: 1,
            case: CaseDefinition {
                id: "IJLO".into(),
                family: "even_jl_no_t".into(),
                source: source(),
            },
            methods: vec![MethodRule {
                id: "save_o".into(),
                source_order: 1,
                category: MethodCategory::SaveO,
                setups: vec![first.clone(), fallback],
            }],
        };
        let mut board = crate::model::empty_board();
        board[0][1] = Some(Piece::O);
        let report = rules.select_for_board(&context(), &board);
        assert_eq!(report.selected.unwrap().setup_id, "fallback");
        assert_eq!(report.candidates[0].status, CandidateStatus::SetupMismatch);
    }

    #[test]
    fn review_required_rule_cannot_be_selected() {
        let mut draft = setup("draft", 1, 1);
        draft.review_status = ReviewStatus::ReviewRequired;
        draft.placements.clear();
        let rules = RuleSet {
            schema_version: 1,
            case: CaseDefinition {
                id: "IJLO".into(),
                family: "even_jl_no_t".into(),
                source: source(),
            },
            methods: vec![MethodRule {
                id: "save_o".into(),
                source_order: 1,
                category: MethodCategory::SaveO,
                setups: vec![draft],
            }],
        };
        rules.validate().unwrap();
        let report = rules.select(&context());
        assert!(report.selected.is_none());
        assert_eq!(report.candidates[0].status, CandidateStatus::ReviewRequired);
    }
}
