use std::sync::atomic::{AtomicBool, Ordering};

use rand::RngCore;

use crate::commands;
use crate::compress::Compression;
use crate::config::{ResolvedRepo, SourceEntry, VykarConfig};
use crate::repo::manifest::SnapshotEntry;
use crate::snapshot::item::Item;
use vykar_types::error::{Result, VykarError};

#[derive(Debug, Clone)]
pub struct BackupSourceResult {
    pub source_label: String,
    pub snapshot_name: String,
    pub source_paths: Vec<String>,
    pub stats: crate::snapshot::SnapshotStats,
}

#[derive(Debug, Clone, Default)]
pub struct BackupRunReport {
    pub created: Vec<BackupSourceResult>,
}

#[derive(Debug, Clone)]
pub struct RepoBackupRunReport {
    pub repo_label: Option<String>,
    pub repository_url: String,
    pub report: BackupRunReport,
}

#[derive(Debug, Clone)]
pub struct RestoreRequest {
    pub snapshot_name: String,
    pub destination: String,
    pub pattern: Option<String>,
}

// ── Full-cycle types ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CycleStep {
    Backup,
    Prune,
    Compact,
    Check,
}

impl CycleStep {
    pub fn command_name(&self) -> &'static str {
        match self {
            Self::Backup => "backup",
            Self::Prune => "prune",
            Self::Compact => "compact",
            Self::Check => "check",
        }
    }
}

#[derive(Debug, Clone)]
pub enum StepOutcome {
    Ok,
    /// Backup completed but some sources had soft errors.
    Partial,
    Skipped(String),
    Failed(String),
}

impl StepOutcome {
    /// Ok and Partial are both "success" — after-hooks run, subsequent steps proceed.
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Ok | Self::Partial)
    }

    pub fn error_msg(&self) -> Option<&str> {
        match self {
            Self::Failed(e) => Some(e),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum CycleEvent {
    StepStarted(CycleStep),
    StepFinished(CycleStep, StepOutcome),
    Backup(commands::backup::BackupProgressEvent),
    Check(commands::check::CheckProgressEvent),
}

pub struct FullCycleResult {
    pub backup_report: Option<BackupRunReport>,
    pub prune_stats: Option<commands::prune::PruneStats>,
    pub compact_stats: Option<commands::compact::CompactStats>,
    pub check_result: Option<commands::check::CheckResult>,
    pub steps: Vec<(CycleStep, StepOutcome)>,
}

impl FullCycleResult {
    /// Any step has Failed outcome.
    pub fn has_failures(&self) -> bool {
        self.steps
            .iter()
            .any(|(_step, o)| matches!(o, StepOutcome::Failed(_)))
    }

    /// Backup step completed with Partial outcome.
    pub fn had_partial(&self) -> bool {
        self.steps
            .iter()
            .any(|(step, o)| matches!((step, o), (CycleStep::Backup, StepOutcome::Partial)))
    }
}

fn generate_snapshot_name() -> String {
    let mut buf = [0u8; 4];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

pub fn run_backup_for_repo(
    config: &VykarConfig,
    sources: &[SourceEntry],
    passphrase: Option<&str>,
) -> Result<BackupRunReport> {
    run_backup_for_repo_with_progress(config, sources, passphrase, &mut |_| {}, None)
}

pub fn run_backup_for_repo_with_progress(
    config: &VykarConfig,
    sources: &[SourceEntry],
    passphrase: Option<&str>,
    progress: &mut dyn FnMut(commands::backup::BackupProgressEvent),
    shutdown: Option<&AtomicBool>,
) -> Result<BackupRunReport> {
    if sources.is_empty() {
        return Err(VykarError::Config(
            "no sources configured for this repository".into(),
        ));
    }

    let compression =
        Compression::from_algorithm(config.compression.algorithm, config.compression.zstd_level);

    let mut report = BackupRunReport::default();

    for source in sources {
        let snapshot_name = generate_snapshot_name();
        let outcome = commands::backup::run_with_progress(
            config,
            commands::backup::BackupRequest {
                snapshot_name: &snapshot_name,
                passphrase,
                source_paths: &source.paths,
                source_label: &source.label,
                exclude_patterns: &source.exclude,
                exclude_if_present: &source.exclude_if_present,
                one_file_system: source.one_file_system,
                git_ignore: source.git_ignore,
                xattrs_enabled: source.xattrs_enabled,
                compression,
                command_dumps: &source.command_dumps,
                verbose: false,
            },
            Some(progress),
            shutdown,
        )?;

        report.created.push(BackupSourceResult {
            source_label: source.label.clone(),
            snapshot_name,
            source_paths: source.paths.clone(),
            stats: outcome.stats,
        });
    }

    Ok(report)
}

/// Run the full backup cycle: backup → prune → compact → check.
///
/// - `before_step`: Called before each step. Return `Ok(())` to proceed,
///   `Err(reason)` to mark the step as Failed (e.g. hook failure).
/// - `after_step`: Called after each step with its outcome.
/// - `on_event`: Progress/lifecycle events for UI updates.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn run_full_cycle_for_repo(
    config: &VykarConfig,
    sources: &[SourceEntry],
    passphrase: Option<&str>,
    shutdown: Option<&AtomicBool>,
    on_event: &mut dyn FnMut(CycleEvent),
    before_step: Option<&mut dyn FnMut(CycleStep) -> std::result::Result<(), String>>,
    after_step: Option<&mut dyn FnMut(CycleStep, &StepOutcome)>,
) -> FullCycleResult {
    let shutting_down = |s: Option<&AtomicBool>| s.is_some_and(|f| f.load(Ordering::SeqCst));

    let mut steps: Vec<(CycleStep, StepOutcome)> = Vec::new();
    let mut backup_report: Option<BackupRunReport> = None;
    let mut prune_stats: Option<commands::prune::PruneStats> = None;
    let mut compact_stats: Option<commands::compact::CompactStats> = None;
    let mut check_result: Option<commands::check::CheckResult> = None;

    // We need mutable access to both callbacks but they're behind Option.
    // Use a helper to split mutable borrows.
    let (before_step, after_step) = {
        // Convert Option<&mut dyn ...> to raw pointers for split borrowing.
        // This is safe because we never hold both references simultaneously.
        (
            before_step.map(|b| b as *mut dyn FnMut(CycleStep) -> std::result::Result<(), String>),
            after_step.map(|a| a as *mut dyn FnMut(CycleStep, &StepOutcome)),
        )
    };

    macro_rules! call_before {
        ($step:expr) => {
            if let Some(ptr) = before_step {
                // SAFETY: we never alias these pointers; only one is called at a time.
                match unsafe { (*ptr)($step) } {
                    Ok(()) => true,
                    Err(reason) => {
                        let outcome = StepOutcome::Failed(reason);
                        on_event(CycleEvent::StepFinished($step, outcome.clone()));
                        if let Some(ptr) = after_step {
                            unsafe { (*ptr)($step, &outcome) };
                        }
                        steps.push(($step, outcome));
                        false
                    }
                }
            } else {
                true
            }
        };
    }

    macro_rules! call_after {
        ($step:expr, $outcome:expr) => {
            if let Some(ptr) = after_step {
                unsafe { (*ptr)($step, $outcome) };
            }
        };
    }

    // 1. Backup
    if !shutting_down(shutdown) {
        let step = CycleStep::Backup;
        on_event(CycleEvent::StepStarted(step));

        if call_before!(step) {
            match run_backup_for_repo_with_progress(
                config,
                sources,
                passphrase,
                &mut |evt| on_event(CycleEvent::Backup(evt)),
                shutdown,
            ) {
                Ok(report) => {
                    let has_errors = report.created.iter().any(|s| s.stats.errors > 0);
                    let outcome = if has_errors {
                        StepOutcome::Partial
                    } else {
                        StepOutcome::Ok
                    };
                    on_event(CycleEvent::StepFinished(step, outcome.clone()));
                    call_after!(step, &outcome);
                    steps.push((step, outcome));
                    backup_report = Some(report);
                }
                Err(e) => {
                    let outcome = StepOutcome::Failed(e.to_string());
                    on_event(CycleEvent::StepFinished(step, outcome.clone()));
                    call_after!(step, &outcome);
                    steps.push((step, outcome));
                }
            }
        }
    }

    let backup_ok = steps
        .iter()
        .any(|(s, o)| matches!(s, CycleStep::Backup) && o.is_success());

    // 2. Prune
    if !shutting_down(shutdown) {
        let step = CycleStep::Prune;
        let has_retention = config.retention.has_any_rule()
            || sources
                .iter()
                .any(|s| s.retention.as_ref().is_some_and(|r| r.has_any_rule()));

        if !has_retention {
            let outcome = StepOutcome::Skipped("no retention rules".into());
            on_event(CycleEvent::StepStarted(step));
            on_event(CycleEvent::StepFinished(step, outcome.clone()));
            steps.push((step, outcome));
        } else if !backup_ok {
            let outcome = StepOutcome::Skipped("backup failed".into());
            on_event(CycleEvent::StepStarted(step));
            on_event(CycleEvent::StepFinished(step, outcome.clone()));
            steps.push((step, outcome));
        } else {
            on_event(CycleEvent::StepStarted(step));
            if call_before!(step) {
                match commands::prune::run(config, passphrase, false, false, sources, &[], shutdown)
                {
                    Ok((stats, _list_entries)) => {
                        prune_stats = Some(stats);
                        let outcome = StepOutcome::Ok;
                        on_event(CycleEvent::StepFinished(step, outcome.clone()));
                        call_after!(step, &outcome);
                        steps.push((step, outcome));
                    }
                    Err(e) => {
                        let outcome = StepOutcome::Failed(e.to_string());
                        on_event(CycleEvent::StepFinished(step, outcome.clone()));
                        call_after!(step, &outcome);
                        steps.push((step, outcome));
                    }
                }
            }
        }
    }

    // 3. Compact
    if !shutting_down(shutdown) {
        let step = CycleStep::Compact;
        if !backup_ok {
            let outcome = StepOutcome::Skipped("backup failed".into());
            on_event(CycleEvent::StepStarted(step));
            on_event(CycleEvent::StepFinished(step, outcome.clone()));
            steps.push((step, outcome));
        } else {
            on_event(CycleEvent::StepStarted(step));
            if call_before!(step) {
                match commands::compact::run(
                    config,
                    passphrase,
                    config.compact.threshold,
                    None,
                    false,
                    shutdown,
                ) {
                    Ok(stats) => {
                        compact_stats = Some(stats);
                        let outcome = StepOutcome::Ok;
                        on_event(CycleEvent::StepFinished(step, outcome.clone()));
                        call_after!(step, &outcome);
                        steps.push((step, outcome));
                    }
                    Err(e) => {
                        let outcome = StepOutcome::Failed(e.to_string());
                        on_event(CycleEvent::StepFinished(step, outcome.clone()));
                        call_after!(step, &outcome);
                        steps.push((step, outcome));
                    }
                }
            }
        }
    }

    // 4. Check (metadata-only)
    if !shutting_down(shutdown) {
        let step = CycleStep::Check;
        on_event(CycleEvent::StepStarted(step));
        if call_before!(step) {
            match commands::check::run_with_progress(
                config,
                passphrase,
                false,
                false,
                Some(&mut |evt| on_event(CycleEvent::Check(evt))),
            ) {
                Ok(result) => {
                    let outcome = if result.errors.is_empty() {
                        StepOutcome::Ok
                    } else {
                        StepOutcome::Failed(format!("check found {} error(s)", result.errors.len()))
                    };
                    check_result = Some(result);
                    on_event(CycleEvent::StepFinished(step, outcome.clone()));
                    call_after!(step, &outcome);
                    steps.push((step, outcome));
                }
                Err(e) => {
                    let outcome = StepOutcome::Failed(e.to_string());
                    on_event(CycleEvent::StepFinished(step, outcome.clone()));
                    call_after!(step, &outcome);
                    steps.push((step, outcome));
                }
            }
        }
    }

    FullCycleResult {
        backup_report,
        prune_stats,
        compact_stats,
        check_result,
        steps,
    }
}

pub fn run_backup_for_all_repos(
    repos: &[ResolvedRepo],
    passphrase_lookup: &mut dyn FnMut(&ResolvedRepo) -> Result<Option<String>>,
) -> Result<Vec<RepoBackupRunReport>> {
    let mut reports = Vec::with_capacity(repos.len());
    for repo in repos {
        let passphrase = passphrase_lookup(repo)?;
        let report = run_backup_for_repo(&repo.config, &repo.sources, passphrase.as_deref())?;
        reports.push(RepoBackupRunReport {
            repo_label: repo.label.clone(),
            repository_url: repo.config.repository.url.clone(),
            report,
        });
    }
    Ok(reports)
}

pub fn list_snapshots(
    config: &VykarConfig,
    passphrase: Option<&str>,
) -> Result<Vec<SnapshotEntry>> {
    commands::list::list_snapshots(config, passphrase)
}

pub fn list_snapshots_with_stats(
    config: &VykarConfig,
    passphrase: Option<&str>,
) -> Result<Vec<(SnapshotEntry, crate::snapshot::SnapshotStats)>> {
    commands::list::list_snapshots_with_stats(config, passphrase)
}

pub fn list_snapshot_items(
    config: &VykarConfig,
    passphrase: Option<&str>,
    snapshot_name: &str,
) -> Result<Vec<Item>> {
    commands::list::list_snapshot_items(config, passphrase, snapshot_name)
}

pub fn restore_snapshot(
    config: &VykarConfig,
    passphrase: Option<&str>,
    req: &RestoreRequest,
) -> Result<commands::restore::RestoreStats> {
    commands::restore::run(
        config,
        passphrase,
        &req.snapshot_name,
        &req.destination,
        req.pattern.as_deref(),
        config.xattrs.enabled,
    )
}

pub fn restore_selected(
    config: &VykarConfig,
    passphrase: Option<&str>,
    snapshot_name: &str,
    destination: &str,
    selected_paths: &std::collections::HashSet<String>,
) -> Result<commands::restore::RestoreStats> {
    commands::restore::run_selected(
        config,
        passphrase,
        snapshot_name,
        destination,
        selected_paths,
        config.xattrs.enabled,
    )
}

pub fn check_repo(
    config: &VykarConfig,
    passphrase: Option<&str>,
    verify_data: bool,
) -> Result<commands::check::CheckResult> {
    commands::check::run(config, passphrase, verify_data, false)
}

pub fn check_repo_with_progress(
    config: &VykarConfig,
    passphrase: Option<&str>,
    verify_data: bool,
    progress: &mut dyn FnMut(commands::check::CheckProgressEvent),
) -> Result<commands::check::CheckResult> {
    commands::check::run_with_progress(config, passphrase, verify_data, false, Some(progress))
}

pub fn delete_snapshot(
    config: &VykarConfig,
    passphrase: Option<&str>,
    snapshot_name: &str,
) -> Result<commands::delete::DeleteStats> {
    commands::delete::run(config, passphrase, snapshot_name, false, None)
}
