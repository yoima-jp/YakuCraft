use std::{
    fs,
    io::{Read, Write},
    path::PathBuf,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use super::{
    ReviewSettingsInput, create_project_core, project_delete_error, project_details_core,
    project_details_core_with_analysis, project_details_from_stored_data,
    review_asset_summary_core, review_asset_summary_core_with_index_and_progress,
    save_review_settings_core, save_review_settings_core_with_cached_asset_index,
    truncate_recent_projects,
};
use crate::{
    commands::{
        editor::update_entries_batch_core,
        export::{
            ExportKindInput, export_language_file_core, export_modified_jar_core,
            export_resource_pack_core, validate_project_core,
        },
    },
    paths::AppPaths,
};
use yc_analysis::{AnalysisStage, CancellationToken};
use yc_project::{
    AppSettings, EntryUpdate, IncludedNamespace, NamespaceRecord, OpenedProject, ProjectError,
    ProjectReviewSettings, RecentProject, RecentProjects, TranslationProgress, UpdateOrigin,
    load_recent_projects, open_project as open_project_file, save_recent_projects,
    save_settings as save_app_settings,
};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn create() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "yakucraft-core-flow-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("temporary directory should be created");
        Self(path)
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn first_mod_unit_id(opened: &OpenedProject) -> i64 {
    opened
        .repository()
        .list_mod_units()
        .expect("mod units should be listed")
        .first()
        .expect("single-mod fixture should have a unit")
        .id
}

#[test]
#[ignore = "requires the external 276-mod BMC3 test pack"]
fn benchmark_large_modpack_flow() {
    use std::{
        collections::{BTreeMap, BTreeSet, HashMap},
        env,
        fs::{self, File},
        io::Read,
        time::Instant,
    };

    use crate::commands::{
        export::{
            ExportKindInput, MergedPackExportOutcome,
            benchmark_export_selected_merged_resource_pack,
            benchmark_export_selected_modified_jars, validate_project_core,
        },
        project::{create_project_from_sources_core, resolve_mods_folder},
    };
    use yc_analysis::{ModLanguageStatus, ScanOptions, scan_mods_folder};
    use yc_project::ProjectEntry;
    use zip::ZipArchive;

    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct EditorEntryPayload<'a> {
        id: i64,
        mod_unit_id: i64,
        namespace: &'a str,
        key: &'a str,
        source_text: Option<&'a str>,
        source_locale: Option<&'a str>,
        target_text: Option<&'a str>,
        origin: &'a yc_language::TranslationOrigin,
        flags: &'a [yc_language::EntryFlag],
    }

    fn ui_payload_size<'a>(entries: impl IntoIterator<Item = &'a ProjectEntry>) -> usize {
        let payload: Vec<_> = entries
            .into_iter()
            .map(|entry| EditorEntryPayload {
                id: entry.id,
                mod_unit_id: entry.mod_unit_id,
                namespace: &entry.namespace,
                key: &entry.key,
                source_text: entry.source_text.as_deref(),
                source_locale: entry.source_locale.as_deref(),
                target_text: entry.target_text.as_deref(),
                origin: &entry.origin,
                flags: &entry.flags,
            })
            .collect();
        serde_json::to_vec(&payload)
            .expect("editor entry DTO should serialize")
            .len()
    }

    fn directory_size(path: &std::path::Path) -> u64 {
        fs::read_dir(path)
            .expect("benchmark directory should be readable")
            .map(|entry| {
                let entry = entry.expect("benchmark directory entry should be readable");
                let metadata = entry
                    .metadata()
                    .expect("benchmark file metadata should be readable");
                if metadata.is_dir() {
                    directory_size(&entry.path())
                } else {
                    metadata.len()
                }
            })
            .sum()
    }

    fn mib(bytes: u64) -> f64 {
        bytes as f64 / (1024.0 * 1024.0)
    }

    let source_instance = env::var_os("YAKUCRAFT_MODPACK_BENCH_INSTANCE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\lw\yc-testpack\bmc3"));
    assert!(
        source_instance.is_dir(),
        "BMC3 instance is missing: {}",
        source_instance.display()
    );

    let resolved = resolve_mods_folder(&source_instance).expect("BMC3 instance should resolve");
    let scan_started = Instant::now();
    let scans = scan_mods_folder(
        &resolved.scanned_folder,
        &ScanOptions::default(),
        &CancellationToken::default(),
        |_| {},
    )
    .expect("BMC3 mods folder should scan");
    let scan_elapsed = scan_started.elapsed();

    let mut classifications = BTreeMap::<&'static str, usize>::new();
    for result in &scans {
        let name = match result.status {
            ModLanguageStatus::Untranslated => "untranslated",
            ModLanguageStatus::Partial { .. } => "partial",
            ModLanguageStatus::Complete => "complete",
            ModLanguageStatus::NoLanguageFiles => "no_language_files",
            ModLanguageStatus::Failed { .. } => "failed",
        };
        *classifications.entry(name).or_default() += 1;
    }
    println!(
        "scan.files={} scan.seconds={:.3} scan.peak_rss_mib={:.1}",
        scans.len(),
        scan_elapsed.as_secs_f64(),
        peak_rss_mib()
    );
    println!("scan.classifications={classifications:?}");
    for failed in scans
        .iter()
        .filter(|result| matches!(result.status, ModLanguageStatus::Failed { .. }))
    {
        if let ModLanguageStatus::Failed { reason } = &failed.status {
            println!("scan.failed file={:?} reason={reason}", failed.file_name);
        }
    }

    let known_parse_error_samples = [
        "FarmersDelight-1.21.1-3.0.3+refabricated.jar",
        "ParticleEffects-1.0.3+1.21.jar",
        "Patchouli-1.21-87-FABRIC.jar",
    ];
    let audit_started = Instant::now();
    let mut analyzer_successes = 0usize;
    for sample in known_parse_error_samples {
        let result = scans
            .iter()
            .find(|result| result.file_name == sample)
            .unwrap_or_else(|| panic!("known malformed-language sample is missing: {sample}"));
        match yc_analysis::analyze_jar(&result.path, &CancellationToken::default(), |_| {}) {
            Ok(analysis) => {
                analyzer_successes += 1;
                println!(
                    "scan.failure_audit file={:?} analyzer=ok languages={} warnings={}",
                    result.file_name,
                    analysis.result.language_files.len(),
                    analysis.result.warnings.len()
                );
            }
            Err(error) => println!(
                "scan.failure_audit file={:?} analyzer=failed reason={error}",
                result.file_name
            ),
        }
    }
    println!(
        "scan.failure_audit.total={} analyzer_successes={} seconds={:.3}",
        known_parse_error_samples.len(),
        analyzer_successes,
        audit_started.elapsed().as_secs_f64()
    );
    for result in scans
        .iter()
        .filter(|result| matches!(result.status, ModLanguageStatus::Failed { .. }))
    {
        match yc_analysis::analyze_jar(&result.path, &CancellationToken::default(), |_| {}) {
            Ok(analysis) => println!(
                "scan.remaining_failure file={:?} analyzer_languages={} usable_languages={} parse_warnings={:?}",
                result.file_name,
                analysis.result.language_files.len(),
                analysis
                    .result
                    .language_files
                    .iter()
                    .filter(|language| language.entry_count > 0)
                    .count(),
                analysis
                    .result
                    .warnings
                    .iter()
                    .filter(|warning| warning
                        .message
                        .starts_with("言語ファイルを解析できませんでした"))
                    .map(|warning| format!("{}: {}", warning.path, warning.message))
                    .collect::<Vec<_>>()
            ),
            Err(error) => println!(
                "scan.remaining_failure file={:?} analyzer=failed reason={error}",
                result.file_name
            ),
        }
    }

    // The scan UI starts at a 100% threshold: only missing or less-than-complete translations are selected.
    let selected: Vec<_> = scans
        .iter()
        .filter(|result| {
            matches!(result.status, ModLanguageStatus::Untranslated)
                || matches!(result.status, ModLanguageStatus::Partial { coverage } if coverage < 1.0)
        })
        .collect();
    assert!(!selected.is_empty(), "the default scan selection is empty");

    let temporary = TemporaryDirectory::create();
    let instance_copy = temporary.0.join("instance-copy");
    let copied_mods = instance_copy.join("mods");
    fs::create_dir_all(&copied_mods).expect("temporary mods folder should be created");
    let source_options = source_instance.join("options.txt");
    if source_options.is_file() {
        fs::copy(&source_options, instance_copy.join("options.txt"))
            .expect("options.txt should be copied");
    }
    let copy_started = Instant::now();
    let source_paths: Vec<_> = selected
        .iter()
        .map(|result| {
            let destination = copied_mods.join(&result.file_name);
            fs::copy(&result.path, &destination).unwrap_or_else(|error| {
                panic!(
                    "failed to copy {} into temporary instance: {error}",
                    result.file_name
                )
            });
            destination
        })
        .collect();
    let copy_elapsed = copy_started.elapsed();
    let instance_copy_bytes = directory_size(&instance_copy);
    println!(
        "instance_copy.mods={} instance_copy.mib={:.1} instance_copy.seconds={:.3}",
        source_paths.len(),
        mib(instance_copy_bytes),
        copy_elapsed.as_secs_f64()
    );

    let data_dir = temporary.0.join("app-data");
    let paths = AppPaths {
        data_dir: data_dir.clone(),
        settings_file: data_dir.join("settings.json"),
        recent_file: data_dir.join("recent.json"),
        logs_dir: data_dir.join("logs"),
        projects_dir: data_dir.join("projects"),
    };
    fs::create_dir_all(&paths.logs_dir).expect("temporary logs directory should be created");
    fs::create_dir_all(&paths.projects_dir)
        .expect("temporary projects directory should be created");

    let create_started = Instant::now();
    let mut unit_started: Option<(usize, String, Instant)> = None;
    let mut unit_times = Vec::<(String, std::time::Duration)>::new();
    let created = create_project_from_sources_core(
        &paths,
        &source_paths,
        Some(&instance_copy),
        &CancellationToken::default(),
        |_, mod_index, _, mod_filename| {
            if unit_started
                .as_ref()
                .is_some_and(|(current_index, _, _)| *current_index != mod_index)
            {
                let (previous_index, previous_name, previous_started) = unit_started
                    .take()
                    .expect("previous mod timing should exist");
                unit_times.push((
                    format!("{previous_index}:{previous_name}"),
                    previous_started.elapsed(),
                ));
            }
            if unit_started.is_none() {
                unit_started = Some((mod_index, mod_filename, Instant::now()));
            }
        },
    )
    .unwrap_or_else(|error| panic!("BMC3 project creation failed: {}", error.user_message));
    let create_elapsed = create_started.elapsed();
    if let Some((mod_index, mod_filename, started)) = unit_started.take() {
        unit_times.push((format!("{mod_index}:{mod_filename}"), started.elapsed()));
    }
    unit_times.sort_by_key(|unit| std::cmp::Reverse(unit.1));
    println!(
        "create.slowest_units={:?}",
        &unit_times[..unit_times.len().min(10)]
    );
    println!(
        "create.units={} create.failures={} create.seconds={:.3} create.peak_rss_mib={:.1} project.mib={:.1}",
        created
            .opened
            .repository()
            .list_mod_units()
            .expect("created units should list")
            .len(),
        created.failed_mods.len(),
        create_elapsed.as_secs_f64(),
        peak_rss_mib(),
        mib(directory_size(created.opened.directory()))
    );
    for failure in &created.failed_mods {
        println!(
            "create.failed file={:?} reason={}",
            failure.filename, failure.reason
        );
    }

    let repository = created.opened.repository();
    let asset_counts = repository
        .asset_type_counts(None)
        .expect("asset type counts should load");
    println!(
        "project.asset_rows={} project.asset_counts={asset_counts:?}",
        asset_counts.values().sum::<usize>()
    );
    let project_file_sizes: BTreeMap<_, _> = fs::read_dir(created.opened.directory())
        .expect("project directory should list")
        .map(|entry| {
            let entry = entry.expect("project directory entry should read");
            let name = entry.file_name().to_string_lossy().into_owned();
            let metadata = entry.metadata().expect("project file metadata should read");
            let bytes = if metadata.is_dir() {
                directory_size(&entry.path())
            } else {
                metadata.len()
            };
            (name, bytes)
        })
        .collect();
    println!("project.top_level_bytes={project_file_sizes:?}");
    let units_started = Instant::now();
    let units = repository
        .list_mod_units()
        .expect("project units should list");
    let mut summary_count = 0usize;
    for unit in &units {
        let _ = repository
            .entry_summary(Some(unit.id))
            .expect("entry summary should load");
        summary_count += 1;
    }
    let units_elapsed = units_started.elapsed();
    println!(
        "list_mod_units.units={} list_mod_units.seconds={:.3} list_mod_units.peak_rss_mib={:.1}",
        summary_count,
        units_elapsed.as_secs_f64(),
        peak_rss_mib()
    );

    let entries_started = Instant::now();
    let entries = repository
        .list_entries(None)
        .expect("project entries should load");
    let entries_elapsed = entries_started.elapsed();
    let mut entries_by_unit = HashMap::<i64, Vec<&ProjectEntry>>::new();
    for entry in &entries {
        entries_by_unit
            .entry(entry.mod_unit_id)
            .or_default()
            .push(entry);
    }
    let mut largest_unit_entries = 0usize;
    let mut largest_unit_payload = 0usize;
    for unit_entries in entries_by_unit.values() {
        largest_unit_entries = largest_unit_entries.max(unit_entries.len());
        largest_unit_payload =
            largest_unit_payload.max(ui_payload_size(unit_entries.iter().copied()));
    }
    let all_payload_bytes = ui_payload_size(entries.iter());
    println!(
        "entries.count={} entries.list_all_seconds={:.3} entries.peak_unit={} entries.max_unit_ipc_bytes={} entries.all_units_ipc_bytes={} entries.peak_rss_mib={:.1}",
        entries.len(),
        entries_elapsed.as_secs_f64(),
        largest_unit_entries,
        largest_unit_payload,
        all_payload_bytes,
        peak_rss_mib()
    );

    let selected_ids: Vec<_> = units.iter().map(|unit| unit.id).collect();
    let validation_started = Instant::now();
    let validation = validate_project_core(
        &created.opened,
        ExportKindInput::ResourcePack,
        Some("1.21.1"),
        None,
        None,
    )
    .unwrap_or_else(|error| panic!("BMC3 project validation failed: {}", error.user_message));
    let validation_elapsed = validation_started.elapsed();
    let mut validation_counts = BTreeMap::<String, usize>::new();
    for issue in &validation.issues {
        *validation_counts
            .entry(format!("{}:{}", issue.severity, issue.code))
            .or_default() += 1;
        if issue.severity == "error" {
            println!(
                "validation.error code={} mod={:?} message={}",
                issue.code, issue.mod_name, issue.message
            );
        }
    }
    println!(
        "validation.issues={} validation.by_severity={validation_counts:?} validation.seconds={:.3} validation.peak_rss_mib={:.1}",
        validation.issues.len(),
        validation_elapsed.as_secs_f64(),
        peak_rss_mib()
    );

    let pack_started = Instant::now();
    let pack_outcome =
        benchmark_export_selected_merged_resource_pack(&created.opened, &selected_ids, "1.21.1")
            .unwrap_or_else(|error| {
                panic!(
                    "BMC3 merged resource pack export failed: {}",
                    error.user_message
                )
            });
    let pack_elapsed = pack_started.elapsed();
    let (pack_path, conflict_count) = match pack_outcome {
        MergedPackExportOutcome::Completed {
            output_path,
            conflicts,
            ..
        } => (PathBuf::from(output_path), conflicts.len()),
        MergedPackExportOutcome::OverwriteRequired { output_path } => {
            panic!("temporary pack unexpectedly required overwrite: {output_path}")
        }
    };
    let project_metadata = repository
        .project_metadata()
        .expect("project metadata should load");
    let expected_namespaces: BTreeSet<_> = project_metadata
        .namespaces
        .iter()
        .filter(|namespace| namespace.included)
        .map(|namespace| namespace.namespace.clone())
        .collect();
    let file = File::open(&pack_path).expect("merged resource pack should exist");
    let mut archive = ZipArchive::new(file).expect("merged resource pack should be a ZIP");
    let mut pack_metadata = None;
    let mut actual_namespaces = BTreeSet::new();
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .expect("resource pack entry should open");
        let name = entry.name().to_owned();
        if name == "pack.mcmeta" {
            let mut bytes = Vec::new();
            entry
                .read_to_end(&mut bytes)
                .expect("pack.mcmeta should read");
            pack_metadata = Some(
                serde_json::from_slice::<serde_json::Value>(&bytes)
                    .expect("pack.mcmeta should be JSON"),
            );
        } else if let Some(namespace) = name
            .strip_prefix("assets/")
            .and_then(|path| path.split_once("/lang/").map(|(namespace, _)| namespace))
        {
            actual_namespaces.insert(namespace.to_owned());
        }
    }
    let pack_format = pack_metadata
        .and_then(|metadata| {
            metadata
                .get("pack")
                .and_then(|pack| pack.get("pack_format"))
                .and_then(serde_json::Value::as_u64)
        })
        .expect("pack.mcmeta should specify pack_format");
    let missing_namespaces: Vec<_> = expected_namespaces.difference(&actual_namespaces).collect();
    assert!(
        missing_namespaces.is_empty(),
        "resource pack is missing namespaces: {missing_namespaces:?}"
    );
    println!(
        "resource_pack.path={} resource_pack.bytes={} resource_pack.pack_format={} resource_pack.namespaces={} resource_pack.expected_namespaces={} resource_pack.conflicts={} resource_pack.seconds={:.3} resource_pack.peak_rss_mib={:.1}",
        pack_path.display(),
        fs::metadata(&pack_path)
            .expect("resource pack metadata should exist")
            .len(),
        pack_format,
        actual_namespaces.len(),
        expected_namespaces.len(),
        conflict_count,
        pack_elapsed.as_secs_f64(),
        peak_rss_mib()
    );

    let modified_directory = temporary.0.join("modified-jars");
    let modified_started = Instant::now();
    let modified_results = benchmark_export_selected_modified_jars(
        &created.opened,
        &selected_ids,
        &modified_directory,
        false,
    )
    .unwrap_or_else(|error| panic!("modified JAR export failed: {}", error.user_message));
    let modified_elapsed = modified_started.elapsed();
    let mut modified_statuses = BTreeMap::<String, usize>::new();
    for result in &modified_results {
        *modified_statuses.entry(result.status.clone()).or_default() += 1;
    }
    let modified_projected: Vec<_> = modified_results
        .iter()
        .filter(|result| result.status == "success")
        .collect();
    for failure in modified_results
        .iter()
        .filter(|result| result.status == "failed")
    {
        println!(
            "modified_jars.failed unit={} mod={:?} reason={:?}",
            failure.mod_unit_id, failure.mod_name, failure.message
        );
    }
    assert!(
        !modified_projected.is_empty(),
        "no modified JAR was exported successfully"
    );
    let sample_modified = PathBuf::from(
        modified_projected[0]
            .output_path
            .as_ref()
            .expect("successful export has a path"),
    );
    let modified_archive =
        yc_archive::Archive::open(&sample_modified).expect("exported JAR should open");
    assert!(
        modified_archive.list_entries().iter().any(|entry| {
            entry
                .normalized_path
                .as_deref()
                .is_some_and(|path| path.starts_with("assets/") && path.contains("/lang/ja_jp."))
        }),
        "exported JAR should contain a Japanese language file"
    );
    println!(
        "modified_jars.results={} modified_jars.statuses={modified_statuses:?} modified_jars.output_mib={:.1} modified_jars.seconds={:.3} modified_jars.peak_rss_mib={:.1}",
        modified_results.len(),
        mib(directory_size(&modified_directory)),
        modified_elapsed.as_secs_f64(),
        peak_rss_mib()
    );

    let replacement_unit_id = modified_projected[0].mod_unit_id;
    let replacement_started = Instant::now();
    let first_replacement = benchmark_export_selected_modified_jars(
        &created.opened,
        &[replacement_unit_id],
        &modified_directory,
        true,
    )
    .unwrap_or_else(|error| {
        panic!(
            "temporary instance JAR replacement failed: {}",
            error.user_message
        )
    });
    assert_eq!(
        first_replacement[0].status, "success",
        "first temporary JAR replacement should succeed: {:?}",
        first_replacement[0].message
    );
    let second_replacement = benchmark_export_selected_modified_jars(
        &created.opened,
        &[replacement_unit_id],
        &modified_directory,
        true,
    )
    .unwrap_or_else(|error| {
        panic!(
            "temporary instance JAR re-export failed: {}",
            error.user_message
        )
    });
    assert_eq!(
        second_replacement[0].status, "success",
        "temporary JAR re-export should succeed: {:?}",
        second_replacement[0].message
    );
    let replacement_elapsed = replacement_started.elapsed();
    let replacements = repository
        .list_jar_replacements(false)
        .expect("replacement record should be readable");
    assert_eq!(
        replacements.len(),
        1,
        "re-export should reuse the original backup record"
    );
    println!(
        "jar_replace.unit={} jar_replace.seconds={:.3} jar_replace.active_records={} jar_replace.peak_rss_mib={:.1} project.final_mib={:.1}",
        replacement_unit_id,
        replacement_elapsed.as_secs_f64(),
        replacements.len(),
        peak_rss_mib(),
        mib(directory_size(created.opened.directory()))
    );

    // Keep the full project analysis alive through export just as the app's project cache does.
    let _analysis_cache_size = created.analyses.len();
}

#[cfg(windows)]
fn peak_rss_mib() -> f64 {
    use std::ffi::c_void;

    #[repr(C)]
    struct ProcessMemoryCountersEx {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
        private_usage: usize,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
    }
    #[link(name = "psapi")]
    unsafe extern "system" {
        fn GetProcessMemoryInfo(
            process: *mut c_void,
            counters: *mut ProcessMemoryCountersEx,
            size: u32,
        ) -> i32;
    }

    let mut counters = ProcessMemoryCountersEx {
        cb: std::mem::size_of::<ProcessMemoryCountersEx>() as u32,
        page_fault_count: 0,
        peak_working_set_size: 0,
        working_set_size: 0,
        quota_peak_paged_pool_usage: 0,
        quota_paged_pool_usage: 0,
        quota_peak_non_paged_pool_usage: 0,
        quota_non_paged_pool_usage: 0,
        pagefile_usage: 0,
        peak_pagefile_usage: 0,
        private_usage: 0,
    };
    let counters_size = counters.cb;
    // These Win32 APIs only read process memory counters and write into the correctly-sized struct.
    let succeeded =
        unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters_size) };
    if succeeded == 0 {
        0.0
    } else {
        counters.peak_working_set_size as f64 / (1024.0 * 1024.0)
    }
}

#[cfg(target_os = "linux")]
fn peak_rss_mib() -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find(|line| line.starts_with("VmHWM:"))
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|kib| kib.parse::<f64>().ok())
        })
        .map(|kib| kib / 1024.0)
        .unwrap_or(0.0)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn peak_rss_mib() -> f64 {
    0.0
}

#[test]
fn scan_folder_resolution_uses_instance_mods_when_selected_folder_has_no_jars() {
    let temporary = TemporaryDirectory::create();
    let instance = temporary.0.join("instance");
    let mods = instance.join("mods");
    fs::create_dir_all(&mods).expect("instance mods folder should be created");
    fs::write(mods.join("sample.jar"), b"fixture").expect("fixture JAR should be written");

    let resolved = super::resolve_mods_folder(&instance)
        .expect("instance folder should resolve to its mods folder");

    assert_eq!(resolved.scanned_folder, mods);
    assert_eq!(resolved.instance_dir, Some(instance));
}

#[test]
fn scan_folder_resolution_keeps_direct_jars_and_reports_empty_mods_folder() {
    let temporary = TemporaryDirectory::create();
    let direct = temporary.0.join("direct");
    fs::create_dir_all(direct.join("mods")).expect("nested mods folder should be created");
    fs::write(direct.join("sample.JAR"), b"fixture").expect("fixture JAR should be written");

    let resolved_direct =
        super::resolve_mods_folder(&direct).expect("folder with direct JARs should resolve");
    assert_eq!(resolved_direct.scanned_folder, direct);

    let empty_instance = temporary.0.join("empty-instance");
    let empty_mods = empty_instance.join("mods");
    fs::create_dir_all(&empty_mods).expect("empty instance mods folder should be created");
    let resolved_empty = super::resolve_mods_folder(&empty_instance)
        .expect("empty instance should still resolve to its mods folder");
    assert_eq!(resolved_empty.scanned_folder, empty_mods);
    assert_eq!(resolved_empty.instance_dir, Some(empty_instance));

    let results = yc_analysis::scan_mods_folder(
        &resolved_empty.scanned_folder,
        &yc_analysis::ScanOptions::default(),
        &CancellationToken::default(),
        |_| {},
    )
    .expect("empty mods folder should scan successfully");
    assert!(results.is_empty());
}

fn included_namespace(mod_unit_id: i64, namespace: &str) -> IncludedNamespace {
    IncludedNamespace {
        mod_unit_id,
        namespace: namespace.to_owned(),
    }
}

#[test]
fn exports_skip_asset_only_namespaces_and_keep_each_language_format() {
    let temporary = TemporaryDirectory::create();
    let source_path = temporary.0.join("export-fixture.jar");
    write_export_namespace_fixture_jar(&source_path);
    let paths = AppPaths::create(temporary.0.join("app-data"))
        .expect("application data directories should be created");
    let created = create_project_core(&paths, &source_path, &CancellationToken::default(), |_| {})
        .expect("export fixture JAR should create a Project");
    let opened = created.opened;
    let mod_unit_id = first_mod_unit_id(&opened);
    let initial_settings = created.analysis.default_review_settings();
    let mut stored_analysis = super::project_analysis_data(
        &created.analysis,
        created.analysis.merge_review(&initial_settings).entries,
        &initial_settings,
    );
    // 以前のProject DBがasset-only namespaceを保存していた状態を再現する。
    stored_analysis.namespaces.push(NamespaceRecord {
        mod_unit_id,
        namespace: "modid".to_owned(),
        container_path: String::new(),
        included: true,
        entry_count: 0,
    });
    opened
        .writer()
        .save_analysis(mod_unit_id, stored_analysis)
        .expect("legacy Project analysis should be restored");
    let mut stale_settings = initial_settings;
    stale_settings.included_namespaces.push("modid".to_owned());
    opened
        .writer()
        .save_review_settings(mod_unit_id, stale_settings, true)
        .expect("legacy namespace selection should be restored");

    let stored_details = super::project_details_from_stored_data(
        &opened,
        opened
            .repository()
            .review_settings()
            .expect("legacy Review settings should load"),
        None,
    )
    .expect("stored project details should load");
    assert!(
        !stored_details
            .included_namespaces
            .iter()
            .any(|namespace| namespace.namespace == "modid")
    );
    assert!(
        !stored_details
            .namespaces
            .iter()
            .any(|namespace| namespace.namespace == "modid")
    );

    let entries = opened
        .repository()
        .list_entries(None)
        .expect("fixture translations should be listed");
    update_entries_batch_core(
        &opened,
        entries
            .into_iter()
            .map(|entry| EntryUpdate {
                id: entry.id,
                target_text: format!("{} translation", entry.namespace),
                origin: UpdateOrigin::Manual,
            })
            .collect(),
    )
    .expect("fixture translations should be saved");

    let details = project_details_core(&opened).expect("Project details should load");
    assert_eq!(
        details
            .namespaces
            .iter()
            .map(|namespace| namespace.namespace.as_str())
            .collect::<Vec<_>>(),
        vec!["fixture", "legacy"]
    );
    assert!(
        !details
            .included_namespaces
            .iter()
            .any(|namespace| namespace.namespace == "modid")
    );
    assert!(
        details
            .namespaces
            .iter()
            .all(|namespace| namespace.entry_count > 0)
    );
    let restored_settings = super::review_settings_from_record(
        &ProjectReviewSettings {
            source_locale: Some("en_us".to_owned()),
            fallback_priority: Vec::new(),
            preserve_existing_japanese: true,
            fill_missing_from_other_languages: true,
            setup_completed: true,
            included_namespaces: vec![
                included_namespace(mod_unit_id, "fixture"),
                included_namespace(mod_unit_id, "legacy"),
                included_namespace(mod_unit_id, "modid"),
            ],
        },
        &created.analysis,
        mod_unit_id,
    );
    assert_eq!(
        restored_settings.included_namespaces,
        vec!["fixture", "legacy"]
    );

    let invalid_namespace = validate_project_core(
        &opened,
        ExportKindInput::LanguageFile,
        None,
        Some("modid"),
        None,
    )
    .expect("namespace validation should complete");
    assert!(
        invalid_namespace
            .issues
            .iter()
            .any(|issue| issue.code == "invalidExportSelection")
    );

    let json_path = temporary.0.join("fixture-ja.json");
    export_language_file_core(&opened, &json_path, "fixture")
        .expect("JSON language file should export");
    assert!(
        fs::read_to_string(&json_path)
            .expect("JSON output should be readable")
            .contains("fixture translation")
    );

    let legacy_path = temporary.0.join("legacy-ja.lang");
    export_language_file_core(&opened, &legacy_path, "legacy")
        .expect("legacy language file should export");
    assert_eq!(
        fs::read_to_string(&legacy_path).expect("legacy output should be readable"),
        "legacy.message=legacy translation\n"
    );

    let modified_validation =
        validate_project_core(&opened, ExportKindInput::ModifiedJar, None, None, None)
            .expect("Modified JAR validation should complete");
    assert!(
        !modified_validation
            .issues
            .iter()
            .any(|issue| issue.severity == "error")
    );
    let modified_path = temporary.0.join("export-fixture-ja.jar");
    export_modified_jar_core(&opened, &modified_path).expect("Modified JAR should export");
    assert!(
        read_zip_entry(&modified_path, "assets/fixture/lang/ja_jp.json")
            .expect("JSON language file should be embedded")
            .windows(b"fixture translation".len())
            .any(|window| window == b"fixture translation")
    );
    assert_eq!(
        read_zip_entry(&modified_path, "assets/legacy/lang/ja_JP.lang")
            .expect("legacy language file should be embedded"),
        b"legacy.message=legacy translation\n"
    );
    assert!(read_zip_entry(&modified_path, "assets/modid/icon.png").is_some());
    assert!(read_zip_entry(&modified_path, "assets/modid/lang/ja_jp.json").is_none());

    let pack_validation = validate_project_core(
        &opened,
        ExportKindInput::ResourcePack,
        Some("26.3"),
        None,
        None,
    )
    .expect("Resource Pack validation should complete");
    assert!(
        !pack_validation
            .issues
            .iter()
            .any(|issue| issue.severity == "error")
    );
    let pack_path = temporary.0.join("export-fixture.zip");
    export_resource_pack_core(&opened, &pack_path, "26.3", None)
        .expect("Resource Pack should export");
    assert!(read_zip_entry(&pack_path, "assets/fixture/lang/ja_jp.json").is_some());
    assert!(read_zip_entry(&pack_path, "assets/legacy/lang/ja_jp.json").is_some());
    assert!(read_zip_entry(&pack_path, "assets/modid/lang/ja_jp.json").is_none());
}

#[test]
fn project_rename_failure_has_actionable_user_message_without_internal_detail() {
    let error = project_delete_error(ProjectError::ProjectDeleteRename(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "internal filesystem detail",
    )));

    assert_eq!(error.code, "project_delete_rename_failed");
    assert_eq!(
        error.user_message,
        "ほかのアプリがファイルを使用しています。しばらくしてからもう一度お試しください。"
    );
    assert!(error.detail.is_none());
}

fn write_fixture_jar(path: &std::path::Path) {
    let file = fs::File::create(path).expect("fixture JAR should be created");
    let mut jar = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (entry_path, contents) in [
        (
            "fabric.mod.json",
            r#"{"schemaVersion":1,"id":"fixture","name":"Fixture Mod","version":"1.2.3","depends":{"minecraft":">=1.20"}}"#,
        ),
        (
            "assets/fixture/lang/en_us.json",
            r#"{"message.hello":"Hello, %s!","item.gear":"Gear","block.fixture.missing_widget":"Missing","block.fixture.machine":"Machine","entity.fixture.creature":"Creature"}"#,
        ),
        (
            "assets/fixture/lang/ja_jp.json",
            r#"{"message.hello":"こんにちは！"}"#,
        ),
        ("assets/fixture/models/block/machine.json", "{}"),
        ("assets/fixture/models/item/gear.json", "{}"),
        (
            "assets/fixture/textures/entity/creature.png",
            "not decoded as an image",
        ),
        ("assets/fixture/geo/creature.geo.json", "{}"),
    ] {
        jar.start_file(entry_path, options)
            .expect("fixture ZIP entry should start");
        jar.write_all(contents.as_bytes())
            .expect("fixture ZIP entry should be written");
    }
    jar.finish().expect("fixture JAR should be finalized");
}

fn write_export_namespace_fixture_jar(path: &std::path::Path) {
    let file = fs::File::create(path).expect("export fixture JAR should be created");
    let mut jar = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (entry_path, contents) in [
        (
            "fabric.mod.json",
            r#"{"schemaVersion":1,"id":"fixture","name":"Fixture Mod","version":"1.0.0"}"#,
        ),
        (
            "assets/fixture/lang/en_us.json",
            r#"{"fixture.message":"Source JSON"}"#,
        ),
        (
            "assets/legacy/lang/en_US.lang",
            "legacy.message=Source legacy\n",
        ),
        // アイコンだけのnamespaceは翻訳候補や各形式の出力対象に含めない。
        ("assets/modid/icon.png", "fixture icon bytes"),
    ] {
        jar.start_file(entry_path, options)
            .expect("export fixture entry should start");
        jar.write_all(contents.as_bytes())
            .expect("export fixture entry should be written");
    }
    jar.finish()
        .expect("export fixture JAR should be finalized");
}

fn read_zip_entry(path: &std::path::Path, name: &str) -> Option<Vec<u8>> {
    let file = fs::File::open(path).expect("exported archive should exist");
    let mut archive = ZipArchive::new(file).expect("exported archive should be valid");
    let Ok(mut entry) = archive.by_name(name) else {
        return None;
    };
    let mut contents = Vec::new();
    entry
        .read_to_end(&mut contents)
        .expect("exported archive entry should be readable");
    Some(contents)
}

fn write_large_analysis_fixture(path: &std::path::Path) {
    let file = fs::File::create(path).expect("large fixture JAR should be created");
    let mut jar = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    jar.start_file("fabric.mod.json", options)
        .expect("metadata entry should start");
    jar.write_all(br#"{"schemaVersion":1,"id":"benchmark","name":"Benchmark","version":"1.0.0"}"#)
        .expect("metadata should be written");

    for namespace_index in 0..5 {
        let mut entries = serde_json::Map::new();
        for entry_index in 0..4_000 {
            entries.insert(
                format!("key.{entry_index:05}"),
                serde_json::Value::String(format!("Value {entry_index}")),
            );
        }
        let encoded = serde_json::to_vec(&entries).expect("encode namespace language file");
        jar.start_file(
            format!("assets/benchmark_{namespace_index}/lang/en_us.json"),
            options,
        )
        .expect("language entry should start");
        jar.write_all(&encoded)
            .expect("language entries should be written");
    }
    jar.finish().expect("large fixture JAR should be finalized");
}

#[test]
fn project_review_edit_and_language_export_use_the_sqlite_data() {
    let temporary = TemporaryDirectory::create();
    let source_path = temporary.0.join("fixture.jar");
    write_fixture_jar(&source_path);
    let paths = AppPaths::create(temporary.0.join("app-data"))
        .expect("application data directories should be created");

    let mut source_removed_during_analysis = false;
    let created = create_project_core(
        &paths,
        &source_path,
        &CancellationToken::default(),
        |progress| {
            if !source_removed_during_analysis && progress.stage == AnalysisStage::ValidateJar {
                fs::remove_file(&source_path)
                    .expect("original JAR should be removable after the Project copy is made");
                source_removed_during_analysis = true;
            }
        },
    )
    .expect("fixture JAR should be analyzed from its Project copy");
    assert!(source_removed_during_analysis);
    assert_eq!(created.mod_version.as_deref(), Some("1.2.3"));
    let opened = created.opened;
    let mod_unit_id = first_mod_unit_id(&opened);
    let project_id = opened.summary().id.clone();

    save_review_settings_core(
        &opened,
        &ReviewSettingsInput {
            source_locale: "en_us".to_owned(),
            fallback_priority: Vec::new(),
            preserve_existing_japanese: true,
            fill_missing_from_other_languages: true,
            included_namespaces: vec![included_namespace(mod_unit_id, "fixture")],
        },
    )
    .expect("Review settings should be merged and saved");

    let review_settings = ReviewSettingsInput {
        source_locale: "en_us".to_owned(),
        fallback_priority: Vec::new(),
        preserve_existing_japanese: true,
        fill_missing_from_other_languages: true,
        included_namespaces: vec![included_namespace(mod_unit_id, "fixture")],
    };
    let selected_assets = review_asset_summary_core(&opened, &review_settings)
        .expect("selected Review namespaces should be counted");
    let empty_selection_assets = review_asset_summary_core(
        &opened,
        &ReviewSettingsInput {
            included_namespaces: Vec::new(),
            ..review_settings
        },
    )
    .expect("empty Review selection should be counted without saving it");
    assert_eq!(selected_assets.assets.unresolved_entries, 1);
    assert_eq!(empty_selection_assets.assets.unresolved_entries, 0);
    assert_eq!(
        opened
            .repository()
            .review_settings()
            .expect("Review selection should remain saved as before")
            .included_namespaces
            .into_iter()
            .map(|selection| selection.namespace)
            .collect::<Vec<_>>(),
        vec!["fixture"]
    );

    let existing = opened
        .repository()
        .list_entries(None)
        .expect("entries should be loaded");
    let greeting = existing
        .iter()
        .find(|entry| entry.key == "message.hello")
        .expect("fixture greeting should be present");
    assert!(
        greeting
            .flags
            .contains(&yc_language::EntryFlag::PlaceholderError),
        "existing Japanese translation with a missing placeholder should be flagged"
    );
    let missing_asset = existing
        .iter()
        .find(|entry| entry.key == "block.fixture.missing_widget")
        .expect("fixture block key should be present");
    assert!(
        missing_asset
            .flags
            .contains(&yc_language::EntryFlag::AssetMissing)
    );

    let validation = validate_project_core(
        &opened,
        ExportKindInput::LanguageFile,
        None,
        Some("fixture"),
        None,
    )
    .expect("project should be validated before export");
    assert!(
        validation
            .issues
            .iter()
            .any(|issue| { issue.code == "placeholderWarning" && issue.severity == "warning" })
    );

    let details = project_details_core(&opened).expect("Project details should load");
    assert_eq!(details.asset_count, Some(4));
    assert_eq!(details.asset_summary.block_models, 1);
    assert_eq!(details.asset_summary.item_models, 1);
    assert_eq!(details.asset_summary.entity_textures, 1);
    assert_eq!(details.asset_summary.geometry_models, 1);
    assert_eq!(details.asset_summary.unresolved_entries, 1);

    let updated = update_entries_batch_core(
        &opened,
        vec![EntryUpdate {
            id: greeting.id,
            target_text: "こんにちは！".to_owned(),
            origin: UpdateOrigin::Manual,
        }],
    )
    .expect("manual edit should be persisted");
    assert!(
        updated[0]
            .flags
            .contains(&yc_language::EntryFlag::PlaceholderError)
    );

    let output_path = temporary.0.join("ja_jp.json");
    export_language_file_core(&opened, &output_path, "fixture")
        .expect("language file should be exported from stored entries");
    let output: serde_json::Value = serde_json::from_slice(
        &fs::read(&output_path).expect("exported language file should exist"),
    )
    .expect("exported language file should be valid JSON");
    assert_eq!(output["message.hello"], "こんにちは！");
    assert!(output.get("item.gear").is_none());
    assert!(
        opened
            .repository()
            .mod_unit_source_path(mod_unit_id)
            .expect("stored source path should load")
            .is_file()
    );
    drop(opened);

    let reopened = open_project_file(
        &paths.projects_dir,
        uuid::Uuid::parse_str(&project_id).expect("Project ID should be a UUID"),
    )
    .expect("Project should reopen after the selected source JAR is removed");
    let reopened_mod_unit_id = first_mod_unit_id(&reopened);
    assert!(
        reopened
            .repository()
            .mod_unit_source_path(reopened_mod_unit_id)
            .expect("reopened stored source path should load")
            .is_file()
    );
}

#[test]
fn review_settings_include_a_namespace_from_a_nested_jar() {
    let temporary = TemporaryDirectory::create();
    let source_path = temporary.0.join("nested-review-fixture.jar");
    write_nested_review_fixture_jar(&source_path);
    let paths = AppPaths::create(temporary.0.join("app-data"))
        .expect("application data directories should be created");
    let created = create_project_core(&paths, &source_path, &CancellationToken::default(), |_| {})
        .expect("nested fixture JAR should create a Project");
    let analysis = created.analysis;
    let opened = created.opened;
    let mod_unit_id = first_mod_unit_id(&opened);
    let nested = opened
        .repository()
        .review_settings()
        .expect("default review settings should be readable");

    let settings = ReviewSettingsInput {
        source_locale: "en_us".to_owned(),
        fallback_priority: Vec::new(),
        preserve_existing_japanese: true,
        fill_missing_from_other_languages: true,
        included_namespaces: nested
            .included_namespaces
            .into_iter()
            .chain(std::iter::once(included_namespace(
                mod_unit_id,
                "nested_fixture",
            )))
            .collect(),
    };
    let (_, prepared_asset_index) = review_asset_summary_core_with_index_and_progress(
        &opened,
        mod_unit_id,
        &settings,
        &analysis,
        |_, _| {},
    )
    .expect("nested namespace review summary should load");
    save_review_settings_core_with_cached_asset_index(
        &opened,
        mod_unit_id,
        &settings,
        &analysis,
        Some(prepared_asset_index),
        true,
        |_, _| {},
    )
    .expect("Review settings with a nested namespace should save");

    let settings = opened
        .repository()
        .review_settings()
        .expect("saved review settings should be readable");
    assert!(
        settings
            .included_namespaces
            .iter()
            .any(|selection| selection.namespace == "nested_fixture")
    );
    assert!(
        opened
            .repository()
            .list_entries(None)
            .expect("merged entries should be readable")
            .iter()
            .any(|entry| entry.namespace == "nested_fixture")
    );
}

fn write_nested_review_fixture_jar(path: &std::path::Path) {
    let nested = {
        let cursor = std::io::Cursor::new(Vec::new());
        let mut jar = ZipWriter::new(cursor);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for (entry_path, contents) in [
            (
                "fabric.mod.json",
                r#"{"schemaVersion":1,"id":"nested_fixture","name":"Nested Fixture","version":"1.0.0"}"#,
            ),
            (
                "assets/nested_fixture/lang/en_us.json",
                r#"{"nested_fixture.message":"Nested message"}"#,
            ),
        ] {
            jar.start_file(entry_path, options)
                .expect("nested fixture entry should start");
            jar.write_all(contents.as_bytes())
                .expect("nested fixture entry should be written");
        }
        jar.finish()
            .expect("nested fixture JAR should be finalized")
            .into_inner()
    };
    let file = fs::File::create(path).expect("root fixture JAR should be created");
    let mut jar = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (entry_path, contents) in [
        (
            "fabric.mod.json",
            r#"{"schemaVersion":1,"id":"root_fixture","name":"Root Fixture","version":"1.0.0","jars":[{"file":"META-INF/jars/nested.jar"}]}"#,
        ),
        (
            "assets/root_fixture/lang/en_us.json",
            r#"{"root_fixture.message":"Root message"}"#,
        ),
    ] {
        jar.start_file(entry_path, options)
            .expect("root fixture entry should start");
        jar.write_all(contents.as_bytes())
            .expect("root fixture entry should be written");
    }
    jar.start_file("META-INF/jars/nested.jar", options)
        .expect("nested archive should start");
    jar.write_all(&nested)
        .expect("nested archive should be written");
    jar.finish().expect("root fixture JAR should be finalized");
}

#[test]
#[ignore = "手動ベンチマーク: Create JARのReview保存とRecent再開の各段階を計測する"]
fn benchmark_create_mod_review_and_recent_open() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .try_init();
    let source_path = std::env::var_os("YAKUCRAFT_FLOW_PERFORMANCE_JAR")
        .map(PathBuf::from)
        .expect("set YAKUCRAFT_FLOW_PERFORMANCE_JAR to the external Mod JAR path");
    assert!(source_path.is_file(), "Mod JAR must exist");
    let temporary = TemporaryDirectory::create();
    let paths = AppPaths::create(temporary.0.join("app-data"))
        .expect("application data directories should be created");

    let started = Instant::now();
    let created = create_project_core(&paths, &source_path, &CancellationToken::default(), |_| {})
        .expect("Create JAR should create a Project");
    println!(
        "Create project analysis and initial save: {:.3}s",
        started.elapsed().as_secs_f64()
    );
    let analysis = std::sync::Arc::clone(&created.analysis);
    let opened = created.opened;
    let mod_unit_id = first_mod_unit_id(&opened);
    let started = Instant::now();
    let details = project_details_core_with_analysis(&opened, mod_unit_id, &analysis)
        .expect("Create Project details should load");
    println!(
        "open_project details: {:.3}s",
        started.elapsed().as_secs_f64()
    );
    println!(
        "Create dataset: {} entries, {} nested namespaces, {} nested archives",
        details.counts.total,
        details
            .namespaces
            .iter()
            .filter(|item| !item.container_path.is_empty())
            .count(),
        details.nested_archives.len()
    );
    let mut included_namespaces = details.included_namespaces.clone();
    let nested_namespace = details
        .namespaces
        .iter()
        .find(|item| !item.container_path.is_empty())
        .expect("Create JAR should contain a nested namespace");
    included_namespaces.push(included_namespace(mod_unit_id, &nested_namespace.namespace));
    let review_settings = ReviewSettingsInput {
        source_locale: details.source_locale,
        fallback_priority: details.fallback_priority,
        preserve_existing_japanese: details.preserve_existing_japanese,
        fill_missing_from_other_languages: details.fill_missing_from_other_languages,
        included_namespaces,
    };

    let started = Instant::now();
    let (summary, prepared_asset_index) = review_asset_summary_core_with_index_and_progress(
        &opened,
        mod_unit_id,
        &review_settings,
        &analysis,
        |_, _| {},
    )
    .expect("nested namespace review summary should load");
    println!(
        "nested selection summary (merge + assets): {:.3}s",
        started.elapsed().as_secs_f64()
    );
    println!("review summary: {summary:?}");
    let started = Instant::now();
    let merge_counts = save_review_settings_core_with_cached_asset_index(
        &opened,
        mod_unit_id,
        &review_settings,
        &analysis,
        Some(prepared_asset_index),
        true,
        |_, _| {},
    )
    .expect("nested namespace review settings should save");
    println!(
        "save_review_settings core: {:.3}s",
        started.elapsed().as_secs_f64()
    );
    let started = Instant::now();
    let saved_review = opened
        .repository()
        .review_settings()
        .expect("saved Review settings should load");
    let mut saved_details =
        project_details_from_stored_data(&opened, saved_review, Some(analysis.result.is_signed))
            .expect("saved Project details should load");
    saved_details.existing_japanese_translation_count = merge_counts.0;
    saved_details.missing_source_keys_count = merge_counts.1;
    println!(
        "save_review_settings details response: {:.3}s",
        started.elapsed().as_secs_f64()
    );

    let project_id =
        uuid::Uuid::parse_str(&opened.summary().id).expect("Project id should be a UUID");
    drop(opened);
    let started = Instant::now();
    let reopened =
        open_project_file(&paths.projects_dir, project_id).expect("Recent Project should reopen");
    println!(
        "open_project database: {:.3}s",
        started.elapsed().as_secs_f64()
    );
    let started = Instant::now();
    let review = reopened
        .repository()
        .review_settings()
        .expect("saved review settings should load");
    let details = project_details_from_stored_data(&reopened, review, None)
        .expect("reopened Project details should load");
    println!(
        "reopened Project details: {:.3}s",
        started.elapsed().as_secs_f64()
    );
    let started = Instant::now();
    let entries = reopened
        .repository()
        .list_entries(None)
        .expect("reopened entries should load");
    println!(
        "list_entries: {:.3}s for {} entries",
        started.elapsed().as_secs_f64(),
        entries.len()
    );
    let started = Instant::now();
    let editor_entries: Vec<_> = entries
        .iter()
        .cloned()
        .map(crate::commands::editor::EditorEntry::from)
        .collect();
    let payload =
        serde_json::to_vec(&editor_entries).expect("editor entries should serialize for IPC");
    println!(
        "list_entries IPC JSON: {:.3}s, {} bytes",
        started.elapsed().as_secs_f64(),
        payload.len()
    );
    assert_eq!(details.project.id, project_id.to_string());
}

#[test]
fn recent_project_retention_keeps_the_newest_configured_count() {
    let mut recent = RecentProjects {
        schema_version: 1,
        projects: ["oldest", "newest", "middle"]
            .into_iter()
            .map(|id| RecentProject {
                id: id.to_owned(),
                display_name: id.to_owned(),
                mod_version: None,
                mod_count: 1,
                instance_path: None,
                last_opened: match id {
                    "newest" => "2026-09-03T00:00:00Z",
                    "middle" => "2026-09-02T00:00:00Z",
                    _ => "2026-09-01T00:00:00Z",
                }
                .to_owned(),
                translation_progress: TranslationProgress::default(),
            })
            .collect(),
    };

    truncate_recent_projects(&mut recent, 2);

    assert_eq!(
        recent
            .projects
            .iter()
            .map(|project| project.id.as_str())
            .collect::<Vec<_>>(),
        vec!["newest", "middle"]
    );
}

#[test]
fn recent_project_count_setting_trims_recent_json() {
    let temporary = TemporaryDirectory::create();
    let paths = AppPaths::create(temporary.0.join("app-data"))
        .expect("application data directories should be created");
    let mut settings = AppSettings::new();
    settings
        .values
        .insert("general.recentProjectsCount".into(), serde_json::json!(2));
    save_app_settings(&paths.settings_file, &settings).expect("setting should be saved");
    let recent = RecentProjects {
        schema_version: 1,
        projects: ["oldest", "newest", "middle"]
            .into_iter()
            .map(|id| RecentProject {
                id: id.to_owned(),
                display_name: id.to_owned(),
                mod_version: None,
                mod_count: 1,
                instance_path: None,
                last_opened: match id {
                    "newest" => "2026-09-03T00:00:00Z",
                    "middle" => "2026-09-02T00:00:00Z",
                    _ => "2026-09-01T00:00:00Z",
                }
                .to_owned(),
                translation_progress: TranslationProgress::default(),
            })
            .collect(),
    };
    save_recent_projects(&paths.recent_file, &recent).expect("recent JSON should be saved");

    super::trim_recent_projects_to_setting(&paths).expect("configured retention should apply");

    let stored = load_recent_projects(&paths.recent_file).expect("recent JSON should be readable");
    assert_eq!(stored.projects.len(), 2);
    assert_eq!(stored.projects[0].id, "newest");
    assert_eq!(stored.projects[1].id, "middle");
}

#[test]
#[ignore = "手動ベンチマーク: fixture と2万キー・5 namespaceのJAR解析時間を計測する"]
fn benchmark_create_project_from_fixture_and_large_jar() {
    let fixture_path = std::env::var_os("YAKUCRAFT_ANALYSIS_BENCHMARK_FIXTURE")
        .map(PathBuf::from)
        .expect("run make_fixture and set YAKUCRAFT_ANALYSIS_BENCHMARK_FIXTURE");
    assert!(fixture_path.is_file(), "fixture JAR must exist");

    let temporary = TemporaryDirectory::create();
    let paths = AppPaths::create(temporary.0.join("app-data"))
        .expect("application data directories should be created");
    let fixture_entries = measure_project_creation("make_fixture", &paths, &fixture_path);
    assert!(fixture_entries > 0);

    let large_fixture = temporary.0.join("large-20k.jar");
    write_large_analysis_fixture(&large_fixture);
    let large_entries =
        measure_project_creation("20,000 keys / 5 namespaces", &paths, &large_fixture);
    assert_eq!(large_entries, 20_000);
}

fn measure_project_creation(
    label: &str,
    paths: &AppPaths,
    fixture_path: &std::path::Path,
) -> usize {
    let started = Instant::now();
    let created = create_project_core(paths, fixture_path, &CancellationToken::default(), |_| {})
        .expect("fixture JAR should create a Project");
    let elapsed = started.elapsed();
    let entry_count = created
        .opened
        .repository()
        .list_entries(None)
        .expect("analyzed entries should be readable")
        .len();
    println!(
        "create_project_from_jar ({label}): {entry_count} entries in {:.3}s",
        elapsed.as_secs_f64()
    );
    drop(created);
    entry_count
}
