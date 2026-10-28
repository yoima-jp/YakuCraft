use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use super::{
    ReviewSettingsInput, create_project_core, merge_for_review, modpack_document_entries,
    preserve_saved_translations, project_analysis_data, project_delete_error, project_details_core,
    project_details_core_with_analysis, project_details_from_stored_data,
    refresh_modpack_unit_core, review_asset_summary_core,
    review_asset_summary_core_with_index_and_progress, save_review_settings_core,
    save_review_settings_core_with_cached_asset_index, truncate_recent_projects,
};
use crate::{
    commands::{
        editor::update_entries_batch_core,
        export::{
            ExportKindInput, ShaderpackExportOutcome, export_language_file_core,
            export_modified_jar_core, export_resource_pack_core, export_selected_shaderpacks_core,
            restore_modpack_file_core, validate_project_core,
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
        false,
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
        let name = entry
            .name()
            .expect("valid resource pack entry name")
            .into_owned();
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

fn write_instance_file(instance: &Path, relative_path: &str, contents: &str) {
    let path = instance.join(relative_path);
    fs::create_dir_all(path.parent().expect("fixture file should have a parent"))
        .expect("fixture directories should be created");
    fs::write(path, contents).expect("fixture file should be written");
}

fn create_modpack_fixture(
    temporary: &TemporaryDirectory,
    files: &[(&str, &str)],
) -> (crate::commands::project::CreatedProject, PathBuf) {
    use crate::commands::project::create_project_from_sources_core;

    let instance = temporary.0.join("instance");
    for (path, contents) in files {
        write_instance_file(&instance, path, contents);
    }
    let paths = AppPaths::create(temporary.0.join("app-data"))
        .expect("application data directories should be created");
    let created = create_project_from_sources_core(
        &paths,
        &[],
        Some(&instance),
        true,
        &CancellationToken::default(),
        |_, _, _, _| {},
    )
    .expect("Modpack fixture should create a project");
    (created, instance)
}

fn translate_modpack_entries(opened: &OpenedProject) {
    let mut updates: Vec<_> = opened
        .repository()
        .list_entries(None)
        .expect("KubeJS entries should be listed")
        .into_iter()
        .map(|entry| EntryUpdate {
            id: entry.id,
            target_text: format!("日本語:{}", entry.source_text.unwrap_or_default()),
            origin: UpdateOrigin::Manual,
        })
        .collect();
    updates.extend(
        opened
            .repository()
            .list_document_entries(None)
            .expect("FTB Quests entries should be listed")
            .into_iter()
            .map(|entry| EntryUpdate {
                id: entry.id,
                target_text: format!("日本語:{}", entry.source_text.unwrap_or_default()),
                origin: UpdateOrigin::Manual,
            }),
    );
    opened
        .writer()
        .update_entries(updates)
        .expect("Modpack translations should be saved");
}

#[test]
fn imports_single_ftb_and_kubejs_and_exports_translations_from_saved_archive() {
    use crate::commands::export::{ModpackFilesExportOutcome, export_modpack_files_core};
    use crate::commands::project::create_project_from_sources_core;
    use std::sync::{Arc, Mutex};
    use yc_modpack::FtbQuestsStatus;
    use zip::ZipArchive;

    let temporary = TemporaryDirectory::create();
    let files = [
        (
            "minecraft/config/ftbquests/quests/lang/en_us.snbt",
            "{ quest.q.title: \"&aEnglish Quest\" }",
        ),
        (
            "minecraft/config/ftbquests/quests/lang/ja_jp.snbt",
            "{ quest.q.title: \"&c既存クエスト\" }",
        ),
        (
            "minecraft/kubejs/assets/example/lang/en_us.json",
            r#"{"example.key":"English KubeJS"}"#,
        ),
        (
            "minecraft/kubejs/assets/example/lang/ja_jp.json",
            r#"{"example.key":"既存KubeJS"}"#,
        ),
    ];
    let instance = temporary.0.join("instance");
    for (path, contents) in &files {
        write_instance_file(&instance, path, contents);
    }
    let scan = yc_modpack::scan_instance(&instance);
    assert_eq!(scan.ftb_quests, FtbQuestsStatus::Single);
    assert_eq!(scan.ftb_quest_entries, 1);
    assert_eq!(scan.kubejs_entries, 1);

    let paths = AppPaths::create(temporary.0.join("app-data")).unwrap();
    let created = create_project_from_sources_core(
        &paths,
        &[],
        Some(&instance),
        true,
        &CancellationToken::default(),
        |_, _, _, _| {},
    )
    .expect("single-file FTB and KubeJS should be imported together");
    let mut opened = created.opened;
    let unit = opened
        .repository()
        .list_mod_units()
        .unwrap()
        .into_iter()
        .next()
        .expect("Modpack unit should be saved");
    assert_eq!(unit.source_kind, yc_project::ModUnitSourceKind::Modpack);
    assert_eq!(
        unit.original_path,
        fs::canonicalize(instance.join("minecraft"))
            .unwrap()
            .to_string_lossy()
            .into_owned()
    );
    let document = opened
        .repository()
        .list_document_entries(Some(unit.id))
        .unwrap();
    assert_eq!(document.len(), 1);
    assert_eq!(document[0].target_text.as_deref(), Some("&c既存クエスト"));
    let validation = validate_project_core(
        &opened,
        ExportKindInput::ResourcePack,
        Some("1.21.1"),
        None,
        None,
    )
    .expect("FTB document placeholders should be validated");
    assert!(validation.issues.iter().any(|issue| {
        issue.code == "placeholderError"
            && issue.severity == "error"
            && issue.namespace.as_deref() == Some("ftbquests")
    }));
    opened
        .writer()
        .save_document_entries(
            unit.id,
            vec![yc_project::DocumentEntryInput {
                namespace: document[0].namespace.clone(),
                key: document[0].key.clone(),
                entry_kind: document[0].entry_kind,
                document_path: document[0].document_path.clone(),
                document_pointer: document[0].document_pointer.clone(),
                source_text: document[0].source_text.clone(),
                source_locale: document[0].source_locale.clone(),
                target_text: None,
                target_original_text: None,
                translated_source_text: None,
                origin: yc_language::TranslationOrigin::Missing,
                sort_index: document[0].sort_index,
                flags: Vec::new(),
            }],
        )
        .expect("disabling existing Japanese import should update the document entry");
    assert!(
        opened
            .repository()
            .list_document_entries(Some(unit.id))
            .unwrap()[0]
            .target_text
            .is_none()
    );
    let kubejs = opened.repository().list_entries(Some(unit.id)).unwrap();
    assert_eq!(kubejs.len(), 1);
    assert_eq!(
        kubejs[0].target_original_text.as_deref(),
        Some("既存KubeJS")
    );

    translate_modpack_entries(&opened);
    let source = opened.repository().mod_unit_source_path(unit.id).unwrap();
    let (analysis, data) = super::super::modpack::analyze_modpack_archive(&source).unwrap();
    let settings = opened
        .repository()
        .review_settings_for_unit(unit.id)
        .unwrap();
    let mut merged = merge_for_review(&analysis, &settings);
    preserve_saved_translations(
        &mut merged.entries,
        opened.repository().list_entries(Some(unit.id)).unwrap(),
    );
    let project_analysis = project_analysis_data(&analysis, merged.entries, &settings);
    opened
        .writer()
        .save_analysis(unit.id, project_analysis)
        .unwrap();
    opened
        .writer()
        .save_document_entries(unit.id, modpack_document_entries(&data, &settings))
        .unwrap();
    assert_eq!(
        opened.repository().list_entries(Some(unit.id)).unwrap()[0]
            .target_text
            .as_deref(),
        Some("日本語:English KubeJS")
    );
    assert_eq!(
        opened
            .repository()
            .list_document_entries(Some(unit.id))
            .unwrap()[0]
            .target_text
            .as_deref(),
        Some("日本語:&aEnglish Quest")
    );
    let resource_pack = temporary.0.join("kubejs-resource-pack.zip");
    export_resource_pack_core(&opened, &resource_pack, "1.21.1", None)
        .expect("KubeJS translations should be included in Resource Packs");
    let kubejs_resource: serde_json::Value = serde_json::from_slice(
        &read_zip_entry(&resource_pack, "assets/example/lang/ja_jp.json")
            .expect("KubeJS locale should be exported to its namespace path"),
    )
    .unwrap();
    assert_eq!(kubejs_resource["example.key"], "日本語:English KubeJS");
    assert!(
        read_zip_entry(
            &resource_pack,
            "minecraft/config/ftbquests/quests/lang/ja_jp.snbt"
        )
        .is_none()
    );
    write_instance_file(
        &instance,
        "minecraft/config/ftbquests/quests/lang/en_us.snbt",
        "{ quest.q.title: \"&aUpdated English Quest\" }",
    );
    write_instance_file(
        &instance,
        "minecraft/kubejs/assets/example/lang/en_us.json",
        r#"{"example.key":"Updated KubeJS source"}"#,
    );
    refresh_modpack_unit_core(&mut opened, unit.id)
        .expect("updated instance files should replace the saved Modpack snapshot");
    let refreshed_source = super::super::modpack::read_modpack_archive(
        &opened.repository().mod_unit_source_path(unit.id).unwrap(),
    )
    .unwrap();
    assert_eq!(
        std::str::from_utf8(&refreshed_source["minecraft/config/ftbquests/quests/lang/en_us.snbt"])
            .unwrap(),
        "{ quest.q.title: \"&aUpdated English Quest\" }"
    );
    assert_eq!(
        opened.repository().list_entries(Some(unit.id)).unwrap()[0]
            .target_text
            .as_deref(),
        Some("日本語:English KubeJS")
    );
    assert_eq!(
        opened
            .repository()
            .list_document_entries(Some(unit.id))
            .unwrap()[0]
            .target_text
            .as_deref(),
        Some("日本語:&aEnglish Quest")
    );
    fs::remove_dir_all(&instance).expect("original instance can be removed after import");
    let output = temporary.0.join("translated-modpack.zip");
    let result = export_modpack_files_core(
        &opened,
        &[unit.id],
        Some(&output),
        false,
        false,
        &Arc::new(Mutex::new(std::collections::HashSet::new())),
    )
    .expect("saved Modpack archive should be sufficient for export");
    assert!(matches!(
        result,
        ModpackFilesExportOutcome::Completed { file_count: 2, .. }
    ));

    let mut archive = ZipArchive::new(fs::File::open(output).unwrap()).unwrap();
    let mut snbt = String::new();
    archive
        .by_name("minecraft/config/ftbquests/quests/lang/ja_jp.snbt")
        .unwrap()
        .read_to_string(&mut snbt)
        .unwrap();
    assert!(snbt.contains("日本語:&aEnglish Quest"));
    let kubejs: serde_json::Value = serde_json::from_reader(
        archive
            .by_name("minecraft/kubejs/assets/example/lang/ja_jp.json")
            .unwrap(),
    )
    .unwrap();
    assert_eq!(kubejs["example.key"], "日本語:English KubeJS");
}

#[test]
fn modpack_refresh_preserves_manually_cleared_kubejs_translation_and_omits_it_on_export() {
    use crate::commands::export::{ModpackFilesExportOutcome, export_modpack_files_core};
    use crate::commands::project::{create_project_from_sources_core, refresh_modpack_unit_core};
    use std::sync::{Arc, Mutex};
    use zip::ZipArchive;

    let temporary = TemporaryDirectory::create();
    let instance = temporary.0.join("instance");
    write_instance_file(
        &instance,
        "minecraft/kubejs/assets/example/lang/en_us.json",
        r#"{"example.key":"English KubeJS","example.untranslated":"Untranslated"}"#,
    );
    write_instance_file(
        &instance,
        "minecraft/kubejs/assets/example/lang/ja_jp.json",
        r#"{"example.key":"既存KubeJS"}"#,
    );

    let paths = AppPaths::create(temporary.0.join("app-data")).unwrap();
    let created = create_project_from_sources_core(
        &paths,
        &[],
        Some(&instance),
        true,
        &CancellationToken::default(),
        |_, _, _, _| {},
    )
    .expect("KubeJS Modpack should be imported");
    let mut opened = created.opened;
    let unit = opened.repository().list_mod_units().unwrap().remove(0);
    let entries = opened.repository().list_entries(Some(unit.id)).unwrap();
    let existing = entries
        .iter()
        .find(|entry| entry.key == "example.key")
        .expect("existing Japanese KubeJS entry should be imported");
    assert_eq!(existing.target_text.as_deref(), Some("既存KubeJS"));

    opened
        .writer()
        .update_entry(yc_project::EntryUpdate {
            id: existing.id,
            target_text: String::new(),
            origin: yc_project::UpdateOrigin::Manual,
        })
        .expect("clearing the translation should be saved as a manual edit");
    write_instance_file(
        &instance,
        "minecraft/kubejs/assets/example/lang/en_us.json",
        r#"{"example.key":"Updated English KubeJS","example.untranslated":"Untranslated"}"#,
    );
    refresh_modpack_unit_core(&mut opened, unit.id)
        .expect("updated KubeJS source should be reimported");

    let refreshed = opened.repository().list_entries(Some(unit.id)).unwrap();
    let cleared = refreshed
        .iter()
        .find(|entry| entry.key == "example.key")
        .expect("cleared KubeJS entry should remain");
    assert_eq!(
        cleared.source_text.as_deref(),
        Some("Updated English KubeJS")
    );
    assert_eq!(cleared.target_text.as_deref(), Some(""));
    assert_eq!(cleared.origin, yc_language::TranslationOrigin::Edited);
    let untranslated = refreshed
        .iter()
        .find(|entry| entry.key == "example.untranslated")
        .expect("untranslated KubeJS entry should remain");
    assert_eq!(untranslated.target_text, None);
    assert_eq!(untranslated.origin, yc_language::TranslationOrigin::Missing);

    let output = temporary.0.join("cleared-modpack.zip");
    let result = export_modpack_files_core(
        &opened,
        &[unit.id],
        Some(&output),
        false,
        false,
        &Arc::new(Mutex::new(std::collections::HashSet::new())),
    )
    .expect("KubeJS with cleared translations should export");
    assert!(matches!(
        result,
        ModpackFilesExportOutcome::Completed { .. }
    ));
    let mut archive = ZipArchive::new(fs::File::open(output).unwrap()).unwrap();
    let kubejs: serde_json::Value = serde_json::from_reader(
        archive
            .by_name("minecraft/kubejs/assets/example/lang/ja_jp.json")
            .unwrap(),
    )
    .unwrap();
    assert!(kubejs.get("example.key").is_none());
}

#[cfg(windows)]
#[test]
fn modpack_refresh_keeps_saved_archive_and_entries_when_a_split_file_cannot_be_read() {
    use std::os::windows::fs::OpenOptionsExt;

    let temporary = TemporaryDirectory::create();
    let files = [
        (
            "minecraft/config/ftbquests/quests/lang/en_us/chapters/first.snbt",
            "{ quest.first.title: \"First Quest\" }",
        ),
        (
            "minecraft/config/ftbquests/quests/lang/en_us/chapters/second.snbt",
            "{ quest.second.title: \"Second Quest\" }",
        ),
    ];
    let (created, instance) = create_modpack_fixture(&temporary, &files);
    let mut opened = created.opened;
    let unit_id = first_mod_unit_id(&opened);
    translate_modpack_entries(&opened);

    let source_path = opened.repository().mod_unit_source_path(unit_id).unwrap();
    let source_before = fs::read(&source_path).unwrap();
    let entries_before: Vec<_> = opened
        .repository()
        .list_document_entries(Some(unit_id))
        .unwrap()
        .into_iter()
        .map(|entry| (entry.id, entry.target_text))
        .collect();
    let unreadable_relative = "minecraft/config/ftbquests/quests/lang/en_us/chapters/first.snbt";
    let unreadable_path = instance.join(unreadable_relative);
    write_instance_file(
        &instance,
        "minecraft/config/ftbquests/quests/lang/en_us/chapters/second.snbt",
        "{ quest.second.title: \"Updated Second Quest\" }",
    );
    let _deny_reads = fs::OpenOptions::new()
        .write(true)
        .share_mode(0)
        .open(&unreadable_path)
        .expect("fixture SNBT file should be locked against reads");

    let error = refresh_modpack_unit_core(&mut opened, unit_id)
        .expect_err("a partial scan must not replace the saved Modpack");
    assert_eq!(error.code, "modpack_scan_incomplete");
    assert!(error.recoverable);
    assert!(
        error.user_message.contains(
            unreadable_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .as_ref()
        ),
        "error should identify the unreadable path: {}",
        error.user_message
    );
    assert_eq!(fs::read(source_path).unwrap(), source_before);
    let entries_after: Vec<_> = opened
        .repository()
        .list_document_entries(Some(unit_id))
        .unwrap()
        .into_iter()
        .map(|entry| (entry.id, entry.target_text))
        .collect();
    assert_eq!(entries_after, entries_before);
}

#[test]
fn modpack_refresh_preserves_excluded_kubejs_namespaces_and_selects_new_ones() {
    let temporary = TemporaryDirectory::create();
    let files = [
        (
            "minecraft/kubejs/assets/excluded/lang/en_us.json",
            r#"{"excluded.key":"Excluded text"}"#,
        ),
        (
            "minecraft/kubejs/assets/included/lang/en_us.json",
            r#"{"included.key":"Included text"}"#,
        ),
    ];
    let (created, instance) = create_modpack_fixture(&temporary, &files);
    let mut opened = created.opened;
    let unit_id = first_mod_unit_id(&opened);
    let source_path = opened.repository().mod_unit_source_path(unit_id).unwrap();
    let (analysis, _) = super::super::modpack::analyze_modpack_archive(&source_path).unwrap();
    let mut settings = analysis.default_review_settings();
    settings
        .included_namespaces
        .retain(|namespace| namespace != "excluded");
    let merged = merge_for_review(&analysis, &settings);
    opened
        .writer()
        .save_analysis(
            unit_id,
            project_analysis_data(&analysis, merged.entries, &settings),
        )
        .unwrap();
    opened
        .writer()
        .save_review_settings(unit_id, settings, true)
        .unwrap();
    assert!(
        opened
            .repository()
            .list_entries(Some(unit_id))
            .unwrap()
            .iter()
            .all(|entry| entry.namespace != "excluded")
    );

    write_instance_file(
        &instance,
        "minecraft/kubejs/assets/new_namespace/lang/en_us.json",
        r#"{"new.key":"New namespace text"}"#,
    );
    refresh_modpack_unit_core(&mut opened, unit_id)
        .expect("the complete Modpack scan should refresh successfully");

    let settings = opened
        .repository()
        .review_settings_for_unit(unit_id)
        .unwrap();
    assert!(
        !settings
            .included_namespaces
            .iter()
            .any(|namespace| namespace == "excluded")
    );
    assert!(
        settings
            .included_namespaces
            .iter()
            .any(|namespace| namespace == "included")
    );
    assert!(
        settings
            .included_namespaces
            .iter()
            .any(|namespace| namespace == "new_namespace")
    );

    let namespaces = opened.repository().project_metadata().unwrap().namespaces;
    assert!(
        !namespaces
            .iter()
            .find(|namespace| namespace.mod_unit_id == unit_id && namespace.namespace == "excluded")
            .unwrap()
            .included
    );
    assert!(
        namespaces
            .iter()
            .find(|namespace| {
                namespace.mod_unit_id == unit_id && namespace.namespace == "new_namespace"
            })
            .unwrap()
            .included
    );
    let entries = opened.repository().list_entries(Some(unit_id)).unwrap();
    assert!(entries.iter().all(|entry| entry.namespace != "excluded"));
    assert!(entries.iter().any(|entry| entry.namespace == "included"));
    assert!(
        entries
            .iter()
            .any(|entry| entry.namespace == "new_namespace")
    );
}

#[test]
fn imports_split_ftb_and_exports_the_matching_ja_jp_tree() {
    use crate::commands::export::{ModpackFilesExportOutcome, export_modpack_files_core};
    use std::sync::{Arc, Mutex};
    use yc_modpack::FtbQuestsStatus;
    use zip::ZipArchive;

    let temporary = TemporaryDirectory::create();
    let files = [
        (
            "minecraft/config/ftbquests/quests/lang/en_us/chapters/start.snbt",
            "{ quest.q.title: \"Split Quest\" }",
        ),
        (
            "minecraft/config/ftbquests/quests/lang/ja_jp/chapters/start.snbt",
            "{ quest.q.title: \"既存の分割訳\" }",
        ),
        (
            "minecraft/kubejs/assets/example/lang/en_us.json",
            r#"{"example.key":"English KubeJS"}"#,
        ),
    ];
    let (created, instance) = create_modpack_fixture(&temporary, &files);
    assert_eq!(
        yc_modpack::scan_instance(&instance).ftb_quests,
        FtbQuestsStatus::Split
    );
    let opened = created.opened;
    let unit_id = opened.repository().list_mod_units().unwrap()[0].id;
    translate_modpack_entries(&opened);

    let output = temporary.0.join("split-translated.zip");
    let result = export_modpack_files_core(
        &opened,
        &[unit_id],
        Some(&output),
        false,
        false,
        &Arc::new(Mutex::new(std::collections::HashSet::new())),
    )
    .unwrap();
    assert!(matches!(
        result,
        ModpackFilesExportOutcome::Completed { .. }
    ));
    let mut archive = ZipArchive::new(fs::File::open(output).unwrap()).unwrap();
    let output_path = "minecraft/config/ftbquests/quests/lang/ja_jp/chapters/start.snbt";
    let mut snbt = String::new();
    archive
        .by_name(output_path)
        .unwrap()
        .read_to_string(&mut snbt)
        .unwrap();
    assert!(snbt.contains("日本語:Split Quest"));
    assert!(
        archive
            .by_name("minecraft/config/ftbquests/quests/lang/ja_jp.snbt")
            .is_err()
    );
}

#[test]
fn modpack_file_validation_checks_final_ftb_and_kubejs_translations() {
    use crate::commands::export::{
        ExportKindInput, ModpackFilesExportOutcome, export_modpack_files_core,
        validate_project_core,
    };
    use std::sync::{Arc, Mutex};
    use zip::ZipArchive;

    let temporary = TemporaryDirectory::create();
    let files = [
        (
            "minecraft/config/ftbquests/quests/lang/en_us.snbt",
            "{ quest.q.title: \"&aQuest {image:example:icon} {@pagebreak}\" }",
        ),
        (
            "minecraft/kubejs/assets/example/lang/en_us.json",
            r#"{"example.key":"Found %s items"}"#,
        ),
    ];
    let (created, _) = create_modpack_fixture(&temporary, &files);
    let opened = created.opened;
    let unit_id = first_mod_unit_id(&opened);
    translate_modpack_entries(&opened);

    let ftb_entry = opened
        .repository()
        .list_document_entries(Some(unit_id))
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let kubejs_entry = opened
        .repository()
        .list_entries(Some(unit_id))
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    opened
        .writer()
        .update_entries(vec![
            EntryUpdate {
                id: ftb_entry.id,
                target_text: "&b翻訳 {image:other:icon} {@pagebreak-extra}".to_owned(),
                origin: UpdateOrigin::Manual,
            },
            EntryUpdate {
                id: kubejs_entry.id,
                target_text: "アイテム".to_owned(),
                origin: UpdateOrigin::Manual,
            },
        ])
        .unwrap();

    let validation =
        validate_project_core(&opened, ExportKindInput::ModpackFiles, None, None, None).unwrap();
    let ftb_issue = validation
        .issues
        .iter()
        .find(|issue| issue.namespace.as_deref() == Some("ftbquests"))
        .expect("the FTB formatting change should be reported");
    assert_eq!(ftb_issue.code, "placeholderError");
    assert_eq!(ftb_issue.severity, "error");
    assert_eq!(
        ftb_issue.target_text.as_deref(),
        Some("&b翻訳 {image:other:icon} {@pagebreak-extra}")
    );
    let kubejs_issue = validation
        .issues
        .iter()
        .find(|issue| issue.namespace.as_deref() == Some("example"))
        .expect("the missing KubeJS argument should be reported");
    assert_eq!(kubejs_issue.code, "placeholderWarning");
    assert_eq!(kubejs_issue.severity, "warning");

    let output = temporary.0.join("validated-modpack.zip");
    let outcome = export_modpack_files_core(
        &opened,
        &[unit_id],
        Some(&output),
        false,
        false,
        &Arc::new(Mutex::new(std::collections::HashSet::new())),
    )
    .unwrap();
    assert!(matches!(
        outcome,
        ModpackFilesExportOutcome::Completed { .. }
    ));
    let mut archive = ZipArchive::new(fs::File::open(output).unwrap()).unwrap();
    let mut ftb_output = String::new();
    archive
        .by_name("minecraft/config/ftbquests/quests/lang/ja_jp.snbt")
        .unwrap()
        .read_to_string(&mut ftb_output)
        .unwrap();
    assert!(ftb_output.contains("&b翻訳 {image:other:icon} {@pagebreak-extra}"));
    let kubejs_output: serde_json::Value = serde_json::from_reader(
        archive
            .by_name("minecraft/kubejs/assets/example/lang/ja_jp.json")
            .unwrap(),
    )
    .unwrap();
    assert_eq!(kubejs_output["example.key"], "アイテム");
}

#[test]
fn direct_modpack_deployment_restores_existing_and_created_files_safely() {
    use crate::commands::export::{
        ModpackFilesExportOutcome, export_modpack_files_core, restore_modpack_file_core,
    };
    use std::sync::{Arc, Mutex};

    let temporary = TemporaryDirectory::create();
    let files = [
        (
            "minecraft/config/ftbquests/quests/lang/en_us.snbt",
            "{ quest.q.title: \"Quest\" }",
        ),
        (
            "minecraft/config/ftbquests/quests/lang/ja_jp.snbt",
            "{ quest.q.title: \"元のクエスト訳\" }",
        ),
        (
            "minecraft/kubejs/assets/example/lang/en_us.json",
            r#"{"example.key":"English"}"#,
        ),
        (
            "minecraft/kubejs/assets/example/lang/ja_jp.json",
            r#"{"example.key":"元のKubeJS訳"}"#,
        ),
        (
            "minecraft/kubejs/assets/created/lang/en_us.json",
            r#"{"created.key":"New file"}"#,
        ),
    ];
    let (created, instance) = create_modpack_fixture(&temporary, &files);
    let opened = created.opened;
    let unit_id = opened.repository().list_mod_units().unwrap()[0].id;
    translate_modpack_entries(&opened);

    let game = instance.join("minecraft");
    let existing_ftb = game.join("config/ftbquests/quests/lang/ja_jp.snbt");
    let original_ftb = fs::read(&existing_ftb).unwrap();
    let existing_kubejs = game.join("kubejs/assets/example/lang/ja_jp.json");
    let original_kubejs = fs::read(&existing_kubejs).unwrap();
    let exported_paths = Arc::new(Mutex::new(std::collections::HashSet::new()));

    let first =
        export_modpack_files_core(&opened, &[unit_id], None, true, false, &exported_paths).unwrap();
    assert!(matches!(
        first,
        ModpackFilesExportOutcome::OverwriteRequired { .. }
    ));
    let outcome =
        export_modpack_files_core(&opened, &[unit_id], None, true, true, &exported_paths).unwrap();
    assert!(matches!(
        outcome,
        ModpackFilesExportOutcome::Completed { file_count: 3, .. }
    ));

    fs::write(&existing_ftb, "user edit").unwrap();
    let records = opened.repository().list_file_deployments(false).unwrap();
    let existing_ftb_path = fs::canonicalize(&existing_ftb)
        .unwrap()
        .to_string_lossy()
        .to_string();
    let changed_id = records
        .iter()
        .find(|record| record.target_path == existing_ftb_path)
        .unwrap()
        .id;
    let changed_error =
        export_modpack_files_core(&opened, &[unit_id], None, true, false, &exported_paths)
            .unwrap_err();
    assert_eq!(changed_error.code, "modpack_file_changed");
    assert!(changed_error.user_message.contains("ja_jp.snbt"));
    assert!(changed_error.user_message.contains("手動で解決"));
    assert_eq!(fs::read(&existing_ftb).unwrap(), b"user edit");
    let forced_error =
        export_modpack_files_core(&opened, &[unit_id], None, true, true, &exported_paths)
            .unwrap_err();
    assert_eq!(forced_error.code, "modpack_file_changed");
    assert_eq!(fs::read(&existing_ftb).unwrap(), b"user edit");
    let restore_error =
        restore_modpack_file_core(&opened, changed_id, &exported_paths).unwrap_err();
    assert_eq!(restore_error.code, "modpack_file_changed");
    assert!(restore_error.user_message.contains("ja_jp.snbt"));
    assert!(restore_error.user_message.contains("手動で解決"));
    assert_eq!(fs::read(&existing_ftb).unwrap(), b"user edit");
    assert_eq!(
        opened
            .repository()
            .list_file_deployments(false)
            .unwrap()
            .len(),
        records.len()
    );
    let backup_path = records
        .iter()
        .find(|record| record.id == changed_id)
        .unwrap()
        .backup_path
        .as_deref()
        .unwrap();
    assert_eq!(fs::read(backup_path).unwrap(), original_ftb);

    let existing_kubejs_path = fs::canonicalize(&existing_kubejs)
        .unwrap()
        .to_string_lossy()
        .to_string();
    let existing_id = records
        .iter()
        .find(|record| record.target_path == existing_kubejs_path)
        .unwrap()
        .id;
    restore_modpack_file_core(&opened, existing_id, &exported_paths).unwrap();
    assert_eq!(fs::read(&existing_kubejs).unwrap(), original_kubejs);

    let created_file = game.join("kubejs/assets/created/lang/ja_jp.json");
    assert!(created_file.is_file());
    let created_file_path = fs::canonicalize(&created_file)
        .unwrap()
        .to_string_lossy()
        .to_string();
    let created_id = records
        .iter()
        .find(|record| record.target_path == created_file_path)
        .unwrap()
        .id;
    restore_modpack_file_core(&opened, created_id, &exported_paths).unwrap();
    assert!(!created_file.exists());
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

fn write_shaderpack_fixture_zip(path: &Path) {
    let file = fs::File::create(path).expect("fixture shaderpack ZIP should be created");
    let mut archive = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (entry_path, contents) in [
        (
            "shaders/lang/en_us.lang",
            "shader.option.existing=Existing\nshader.option.format=§aColor\nshader.option.blank=Blank\n",
        ),
        ("shaders/lang/ja_jp.lang", "shader.option.existing=既存訳\n"),
        ("shaders/shaders.properties", "oldLighting=true\n"),
    ] {
        archive
            .start_file(entry_path, options)
            .expect("fixture shaderpack entry should start");
        archive
            .write_all(contents.as_bytes())
            .expect("fixture shaderpack entry should be written");
    }
    archive
        .finish()
        .expect("fixture shaderpack ZIP should be finalized");
}

fn create_shaderpack_project(
    path: &Path,
    instance_path: Option<&Path>,
) -> crate::commands::project::CreatedProject {
    let source_path = path.join("fixture-shaders.zip");
    write_shaderpack_fixture_zip(&source_path);
    let app_paths = AppPaths::create(path.join("app-data"))
        .expect("application data directories should be created");
    super::create_project_from_sources_core(
        &app_paths,
        &[source_path],
        instance_path,
        false,
        &CancellationToken::default(),
        |_, _, _, _| {},
    )
    .expect("shaderpack should be imported")
}

#[test]
fn shaderpack_import_translation_and_zip_export_use_shader_lang_format() {
    let temporary = TemporaryDirectory::create();
    let source_path = temporary.0.join("fixture-shaders.zip");
    write_shaderpack_fixture_zip(&source_path);
    assert!(
        yc_shaderpack::is_shader_pack(&yc_shaderpack::ShaderPackSource::Zip(source_path.clone()))
            .expect("fixture ZIP should be inspected")
    );
    yc_analysis::analyze_shaderpack(&source_path, "shaderpack-fixture-shaders")
        .expect("fixture ZIP should analyze as a shaderpack");
    let app_paths = AppPaths::create(temporary.0.join("app-data"))
        .expect("application data directories should be created");
    let created = super::create_project_from_sources_core(
        &app_paths,
        std::slice::from_ref(&source_path),
        None,
        false,
        &CancellationToken::default(),
        |_, _, _, _| {},
    )
    .expect("shaderpack should be imported");
    let opened = created.opened;
    let mod_unit_id = first_mod_unit_id(&opened);
    let mut entries = opened
        .repository()
        .list_entries(Some(mod_unit_id))
        .expect("shaderpack translations should load");
    assert_eq!(entries.len(), 3);
    assert_eq!(
        entries
            .iter()
            .find(|entry| entry.key == "shader.option.existing")
            .and_then(|entry| entry.target_text.as_deref()),
        Some("既存訳")
    );
    for entry in &mut entries {
        let target_text = match entry.key.as_str() {
            "shader.option.existing" => "既存訳",
            "shader.option.format" => "§a色",
            "shader.option.blank" => " \t ",
            _ => unreachable!("only shaderpack fixture entries should exist"),
        };
        entry.target_text = Some(target_text.to_owned());
    }
    update_entries_batch_core(
        &opened,
        entries
            .iter()
            .map(|entry| EntryUpdate {
                id: entry.id,
                target_text: entry.target_text.clone().unwrap_or_default(),
                origin: UpdateOrigin::Manual,
            })
            .collect(),
    )
    .expect("shaderpack translations should save");

    let original_bytes = fs::read(&source_path).expect("read original shaderpack");
    let output_directory = temporary.0.join("exports");
    let outcome = export_selected_shaderpacks_core(
        &opened,
        &[mod_unit_id],
        Some(&output_directory),
        false,
        false,
        &std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
    )
    .expect("translated shaderpack should export");
    let output_paths = match outcome {
        ShaderpackExportOutcome::Completed { output_paths, .. } => output_paths,
        ShaderpackExportOutcome::OverwriteRequired { .. } => {
            panic!("fresh export folder should not require overwrite")
        }
    };
    assert_eq!(output_paths.len(), 1);
    let output_path = PathBuf::from(&output_paths[0]);
    let japanese_lang = read_zip_entry(&output_path, "shaders/lang/ja_jp.lang")
        .expect("Japanese shader language should be in the output ZIP");
    let parsed = yc_shaderpack::parse_lang(&japanese_lang);
    assert!(
        parsed
            .entries
            .contains(&("shader.option.existing".to_owned(), "既存訳".to_owned()))
    );
    assert!(
        parsed
            .entries
            .contains(&("shader.option.format".to_owned(), "§a色".to_owned()))
    );
    assert!(
        !parsed
            .entries
            .iter()
            .any(|(key, _)| key == "shader.option.blank")
    );
    assert_eq!(
        fs::read(&source_path).expect("original shaderpack should remain"),
        original_bytes
    );
}

#[test]
fn shaderpack_export_disambiguates_case_insensitive_names() {
    let temporary = TemporaryDirectory::create();
    let first_dir = temporary.0.join("first");
    let second_dir = temporary.0.join("second");
    fs::create_dir_all(&first_dir).unwrap();
    fs::create_dir_all(&second_dir).unwrap();
    let sources = [first_dir.join("Pack.zip"), second_dir.join("pack.zip")];
    for source in &sources {
        write_shaderpack_fixture_zip(source);
    }
    let app_paths = AppPaths::create(temporary.0.join("app-data")).unwrap();
    let opened = super::create_project_from_sources_core(
        &app_paths,
        &sources,
        None,
        false,
        &CancellationToken::default(),
        |_, _, _, _| {},
    )
    .unwrap()
    .opened;
    let ids: Vec<_> = opened
        .repository()
        .list_mod_units()
        .unwrap()
        .iter()
        .map(|unit| unit.id)
        .collect();
    assert_eq!(ids.len(), 2);
    let outcome = export_selected_shaderpacks_core(
        &opened,
        &ids,
        Some(&temporary.0.join("exports")),
        false,
        false,
        &std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
    )
    .unwrap();
    let ShaderpackExportOutcome::Completed { output_paths, .. } = outcome else {
        panic!("fresh export folder should not require overwrite");
    };
    assert_eq!(output_paths.len(), 2);
    assert_ne!(
        output_paths[0].to_lowercase(),
        output_paths[1].to_lowercase()
    );
    for path in output_paths {
        assert!(Path::new(&path).is_file());
        assert!(read_zip_entry(Path::new(&path), "shaders/lang/ja_jp.lang").is_some());
    }
}

#[test]
fn shaderpack_instance_deployment_can_be_restored() {
    let temporary = TemporaryDirectory::create();
    let instance = temporary.0.join("instance");
    let shaderpacks = instance.join("shaderpacks");
    fs::create_dir_all(&shaderpacks).expect("instance shaderpacks directory should exist");
    let deployed_name = "fixture-shaders-ja.zip";
    let target = shaderpacks.join(deployed_name);
    let original_target = b"existing shaderpack file";
    fs::write(&target, original_target).expect("write preexisting target");

    let created = create_shaderpack_project(&temporary.0, Some(&instance));
    let opened = created.opened;
    let mod_unit_id = first_mod_unit_id(&opened);
    let exports = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
    let first =
        export_selected_shaderpacks_core(&opened, &[mod_unit_id], None, true, false, &exports)
            .expect("existing target should request overwrite confirmation");
    assert!(matches!(
        first,
        ShaderpackExportOutcome::OverwriteRequired { .. }
    ));
    let installed =
        export_selected_shaderpacks_core(&opened, &[mod_unit_id], None, true, true, &exports)
            .expect("confirmed shaderpack should deploy");
    assert!(matches!(
        installed,
        ShaderpackExportOutcome::Completed {
            directly_installed: true,
            ..
        }
    ));
    assert_ne!(
        fs::read(&target).expect("read deployed output"),
        original_target
    );

    restore_modpack_file_core(
        &opened,
        opened
            .repository()
            .list_file_deployments(false)
            .expect("list active deployments")[0]
            .id,
        &exports,
    )
    .expect("shaderpack deployment should restore");
    assert_eq!(
        fs::read(&target).expect("read restored target"),
        original_target
    );
    assert!(
        opened
            .repository()
            .list_file_deployments(false)
            .expect("list active deployments")
            .is_empty()
    );
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
fn opening_legacy_project_restores_patchouli_validation_for_edit_and_export() {
    let temporary = TemporaryDirectory::create();
    let source_path = temporary.0.join("patchouli-i18n-fixture.jar");
    let file = fs::File::create(&source_path).expect("fixture JAR should be created");
    let mut jar = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (path, contents) in [
        (
            "fabric.mod.json",
            r#"{"schemaVersion":1,"id":"fixture","name":"Fixture","version":"1.0.0"}"#,
        ),
        (
            "data/fixture/patchouli_books/guide/book.json",
            r#"{"i18n":true,"landing_text":"book.landing"}"#,
        ),
        (
            "assets/fixture/lang/en_us.json",
            r#"{"book.landing":"Welcome $(bold)world$(/bold)"}"#,
        ),
    ] {
        jar.start_file(path, options)
            .expect("fixture ZIP entry should start");
        jar.write_all(contents.as_bytes())
            .expect("fixture ZIP entry should be written");
    }
    jar.finish().expect("fixture JAR should be finalized");

    let paths = AppPaths::create(temporary.0.join("app-data"))
        .expect("application data directories should be created");
    let created = create_project_core(&paths, &source_path, &CancellationToken::default(), |_| {})
        .expect("Patchouli fixture should create a Project");
    let opened = created.opened;
    let mod_unit_id = first_mod_unit_id(&opened);
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
    .expect("completed Review settings should be saved");
    let entry = opened
        .repository()
        .list_entries(Some(mod_unit_id))
        .expect("fixture entry should be listed")
        .into_iter()
        .find(|entry| entry.key == "book.landing")
        .expect("Patchouli i18n lang entry should exist");
    assert!(entry.flags.contains(&yc_language::EntryFlag::PatchouliText));
    opened
        .writer()
        .update_entry(EntryUpdate {
            id: entry.id,
            target_text: "ようこそ $(bold)世界$(/bold)".to_owned(),
            origin: UpdateOrigin::Manual,
        })
        .expect("valid translation should be saved");
    // v1.0.0 projects had no patchouli_text rows in entry_flags.
    opened
        .writer()
        .remove_flag(entry.id, yc_language::EntryFlag::PatchouliText)
        .expect("legacy flag should be removed from the fixture");
    let entry_id = entry.id;
    let project_id = uuid::Uuid::parse_str(&opened.summary().id).expect("Project UUID");
    drop(opened);

    let reopened =
        open_project_file(&paths.projects_dir, project_id).expect("legacy project should open");
    let restored = reopened
        .repository()
        .list_entries(Some(mod_unit_id))
        .expect("reopened entries should be listed")
        .into_iter()
        .find(|entry| entry.id == entry_id)
        .expect("saved entry should remain available");
    assert!(
        restored
            .flags
            .contains(&yc_language::EntryFlag::PatchouliText)
    );

    let edited = update_entries_batch_core(
        &reopened,
        vec![EntryUpdate {
            id: restored.id,
            target_text: "ようこそ 世界".to_owned(),
            origin: UpdateOrigin::Manual,
        }],
    )
    .expect("manual edit should be validated");
    assert!(
        edited[0]
            .flags
            .contains(&yc_language::EntryFlag::PlaceholderError)
    );

    let validation = validate_project_core(
        &reopened,
        ExportKindInput::LanguageFile,
        None,
        Some("fixture"),
        None,
    )
    .expect("export validation should complete");
    assert!(
        validation
            .issues
            .iter()
            .any(|issue| { issue.code == "placeholderWarning" && issue.severity == "warning" })
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

#[test]
fn imports_edits_and_exports_patchouli_documents_without_changing_other_json() {
    let temporary = TemporaryDirectory::create();
    let source_path = temporary.0.join("patchouli-only.jar");
    let file = fs::File::create(&source_path).expect("Patchouli fixture JAR should be created");
    let mut jar = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (entry_path, contents) in [
        (
            "fabric.mod.json",
            r#"{"schemaVersion":1,"id":"guidebook","name":"Guidebook","version":"1.0","depends":{"minecraft":">=1.20"}}"#,
        ),
        (
            "assets/guidebook/patchouli_books/guide/book.json",
            r#"{"name":"Guide","i18n":false}"#,
        ),
        (
            "assets/guidebook/patchouli_books/guide/en_us/entries/basics/start.json",
            r#"{"name":"Getting Started","pages":[{"type":"patchouli:text","text":"$(item) Read %s","title":"Welcome"}],"icon":"minecraft:book","unknown":{"kept":true}}"#,
        ),
        (
            "assets/guidebook/patchouli_books/guide/ja_jp/entries/basics/start.json",
            r#"{"name":"既存タイトル","pages":[{"type":"patchouli:text","text":"既存本文","title":"既存見出し"}],"icon":"minecraft:book"}"#,
        ),
    ] {
        jar.start_file(entry_path, options)
            .expect("Patchouli fixture entry should start");
        jar.write_all(contents.as_bytes())
            .expect("Patchouli fixture entry should be written");
    }
    jar.finish()
        .expect("Patchouli fixture JAR should be finalized");

    let paths = AppPaths::create(temporary.0.join("app-data"))
        .expect("application data directories should be created");
    let created = create_project_core(&paths, &source_path, &CancellationToken::default(), |_| {})
        .expect("book-only JAR should create a project");
    let opened = created.opened;
    let mod_unit_id = first_mod_unit_id(&opened);
    let analyzed_details =
        project_details_core_with_analysis(&opened, mod_unit_id, &created.analysis)
            .expect("Patchouli-only namespace should appear after analysis");
    assert!(
        analyzed_details
            .namespaces
            .iter()
            .any(|namespace| namespace.namespace == "guidebook")
    );
    assert!(
        analyzed_details
            .included_namespaces
            .iter()
            .any(|namespace| namespace.namespace == "guidebook")
    );
    let stored_review = opened
        .repository()
        .review_settings()
        .expect("stored Review settings should load");
    let stored_details = project_details_from_stored_data(&opened, stored_review, None)
        .expect("Patchouli-only namespace should appear in stored details");
    assert!(
        stored_details
            .namespaces
            .iter()
            .any(|namespace| namespace.namespace == "guidebook")
    );
    assert!(
        stored_details
            .included_namespaces
            .iter()
            .any(|namespace| namespace.namespace == "guidebook")
    );
    save_review_settings_core(
        &opened,
        &ReviewSettingsInput {
            source_locale: stored_details.source_locale.clone(),
            fallback_priority: stored_details.fallback_priority.clone(),
            preserve_existing_japanese: stored_details.preserve_existing_japanese,
            fill_missing_from_other_languages: stored_details.fill_missing_from_other_languages,
            included_namespaces: stored_details.included_namespaces.clone(),
        },
    )
    .expect("saving the stored namespace selection should preserve Patchouli rows");
    assert_eq!(
        opened
            .repository()
            .list_document_entries(Some(mod_unit_id))
            .expect("Patchouli rows should remain after saving selection")
            .len(),
        3
    );
    let mut entries = opened
        .repository()
        .list_document_entries(Some(mod_unit_id))
        .expect("Patchouli document entries should be stored");
    assert_eq!(entries.len(), 3);
    let existing_text = entries
        .iter()
        .find(|entry| entry.document_pointer == "/pages/0/text")
        .expect("existing Japanese page should be imported");
    assert_eq!(existing_text.target_text.as_deref(), Some("既存本文"));
    assert_eq!(
        existing_text.origin,
        yc_language::TranslationOrigin::Existing
    );
    let settings = opened.repository().review_settings().unwrap();
    save_review_settings_core(
        &opened,
        &ReviewSettingsInput {
            source_locale: settings.source_locale.unwrap_or_else(|| "en_us".to_owned()),
            fallback_priority: settings.fallback_priority,
            preserve_existing_japanese: false,
            fill_missing_from_other_languages: settings.fill_missing_from_other_languages,
            included_namespaces: settings.included_namespaces,
        },
    )
    .expect("disabling Japanese import should refresh Patchouli document entries");
    entries = opened
        .repository()
        .list_document_entries(Some(mod_unit_id))
        .expect("updated Patchouli entries should be stored");
    let text_entry_id = entries
        .iter_mut()
        .find(|entry| entry.document_pointer == "/pages/0/text")
        .expect("page text should be extracted")
        .id;
    let text_entry = entries
        .iter()
        .find(|entry| entry.document_pointer == "/pages/0/text")
        .unwrap();
    assert_eq!(text_entry.target_text, None);
    assert_eq!(text_entry.origin, yc_language::TranslationOrigin::Missing);
    assert!(entries.iter().all(|entry| {
        entry.entry_kind == yc_project::TranslationEntryKind::Patchouli
            && entry.namespace == "guidebook"
    }));
    update_entries_batch_core(
        &opened,
        vec![EntryUpdate {
            id: text_entry_id,
            target_text: "読んでください $(item) %s".to_owned(),
            origin: UpdateOrigin::Manual,
        }],
    )
    .expect("Patchouli translation should be saved");
    let valid_text = opened
        .repository()
        .list_document_entries(Some(mod_unit_id))
        .unwrap()
        .into_iter()
        .find(|entry| entry.id == text_entry_id)
        .unwrap();
    assert!(
        !valid_text
            .flags
            .contains(&yc_language::EntryFlag::PlaceholderError)
    );
    let settings = opened.repository().review_settings().unwrap();
    save_review_settings_core(
        &opened,
        &ReviewSettingsInput {
            source_locale: settings.source_locale.unwrap_or_else(|| "en_us".to_owned()),
            fallback_priority: settings.fallback_priority,
            preserve_existing_japanese: true,
            fill_missing_from_other_languages: settings.fill_missing_from_other_languages,
            included_namespaces: settings.included_namespaces,
        },
    )
    .expect("reanalysis should preserve a saved Patchouli translation");
    let saved_text = opened
        .repository()
        .list_document_entries(Some(mod_unit_id))
        .unwrap()
        .into_iter()
        .find(|entry| entry.document_pointer == "/pages/0/text")
        .unwrap();
    assert_eq!(
        saved_text.target_text.as_deref(),
        Some("読んでください $(item) %s")
    );
    assert!(
        !saved_text
            .flags
            .contains(&yc_language::EntryFlag::PlaceholderError)
    );
    update_entries_batch_core(
        &opened,
        vec![EntryUpdate {
            id: text_entry_id,
            target_text: "読んでください $(thing) %s".to_owned(),
            origin: UpdateOrigin::Manual,
        }],
    )
    .expect("invalid Patchouli placeholders should be flagged");
    let settings = opened.repository().review_settings().unwrap();
    save_review_settings_core(
        &opened,
        &ReviewSettingsInput {
            source_locale: settings.source_locale.unwrap_or_else(|| "en_us".to_owned()),
            fallback_priority: settings.fallback_priority,
            preserve_existing_japanese: true,
            fill_missing_from_other_languages: settings.fill_missing_from_other_languages,
            included_namespaces: settings.included_namespaces,
        },
    )
    .expect("reanalysis should revalidate the retained Patchouli translation");
    let saved_text = opened
        .repository()
        .list_document_entries(Some(mod_unit_id))
        .unwrap()
        .into_iter()
        .find(|entry| entry.document_pointer == "/pages/0/text")
        .unwrap();
    assert!(
        saved_text
            .flags
            .contains(&yc_language::EntryFlag::PlaceholderError)
    );
    let validation = validate_project_core(
        &opened,
        ExportKindInput::ResourcePack,
        Some("1.21.1"),
        None,
        None,
    )
    .expect("Patchouli document placeholders should be validated");
    assert!(validation.issues.iter().any(|issue| {
        issue.code == "placeholderError"
            && issue.severity == "error"
            && issue.namespace.as_deref() == Some("guidebook")
    }));

    update_entries_batch_core(
        &opened,
        vec![EntryUpdate {
            id: text_entry_id,
            target_text: String::new(),
            origin: UpdateOrigin::Manual,
        }],
    )
    .expect("clearing Patchouli page text should be saved");

    let pack_path = temporary.0.join("patchouli-only.zip");
    export_resource_pack_core(&opened, &pack_path, "1.21.1", None)
        .expect("Patchouli Resource Pack should export");
    let target_path = "assets/guidebook/patchouli_books/guide/ja_jp/entries/basics/start.json";
    let output: serde_json::Value = serde_json::from_slice(
        &read_zip_entry(&pack_path, target_path).expect("translated Patchouli JSON should exist"),
    )
    .expect("translated Patchouli document should be valid JSON");
    let expected: serde_json::Value = serde_json::from_str(
        r#"{"name":"既存タイトル","pages":[{"type":"patchouli:text","text":"$(item) Read %s","title":"既存見出し"}],"icon":"minecraft:book","unknown":{"kept":true}}"#,
    )
    .expect("merged Patchouli JSON should be valid");
    assert_eq!(output, expected);
}

#[test]
fn manual_edits_use_patchouli_profile_for_i18n_language_entries() {
    let temporary = TemporaryDirectory::create();
    let source_path = temporary.0.join("patchouli-i18n-fixture.jar");
    let file = fs::File::create(&source_path).expect("Patchouli i18n fixture should be created");
    let mut jar = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (entry_path, contents) in [
        (
            "fabric.mod.json",
            r#"{"schemaVersion":1,"id":"guidebook","name":"Guidebook","version":"1.0"}"#,
        ),
        (
            "assets/guidebook/patchouli_books/guide/book.json",
            r#"{"name":"patchouli.guidebook.name","i18n":true}"#,
        ),
        (
            "assets/guidebook/patchouli_books/guide/en_us/entries/start.json",
            r#"{"name":"patchouli.guidebook.start.name","pages":[{"type":"patchouli:text","text":"patchouli.guidebook.start.text"}]}"#,
        ),
        (
            "assets/guidebook/lang/en_us.json",
            r#"{"patchouli.guidebook.name":"Guide","patchouli.guidebook.start.name":"$(item) Start","patchouli.guidebook.start.text":"$(thing) Read %s"}"#,
        ),
        (
            "assets/guidebook/lang/ja_jp.json",
            r#"{"patchouli.guidebook.name":"ガイド","patchouli.guidebook.start.name":"開始","patchouli.guidebook.start.text":"読む %s"}"#,
        ),
    ] {
        jar.start_file(entry_path, options)
            .expect("Patchouli i18n fixture entry should start");
        jar.write_all(contents.as_bytes())
            .expect("Patchouli i18n fixture entry should be written");
    }
    jar.finish()
        .expect("Patchouli i18n fixture should be finalized");

    let paths = AppPaths::create(temporary.0.join("app-data"))
        .expect("application data directories should be created");
    let created = create_project_core(&paths, &source_path, &CancellationToken::default(), |_| {})
        .expect("Patchouli i18n JAR should create a project");
    let opened = created.opened;
    let entry = opened
        .repository()
        .list_entries(None)
        .expect("Patchouli i18n language entries should be listed")
        .into_iter()
        .find(|entry| entry.key == "patchouli.guidebook.start.name")
        .expect("Patchouli i18n key should be imported");
    assert!(entry.flags.contains(&yc_language::EntryFlag::PatchouliText));
    assert!(
        entry
            .flags
            .contains(&yc_language::EntryFlag::PlaceholderError)
    );

    let invalid = update_entries_batch_core(
        &opened,
        vec![EntryUpdate {
            id: entry.id,
            target_text: "開始".to_owned(),
            origin: UpdateOrigin::Manual,
        }],
    )
    .expect("manual edit should validate Patchouli placeholders");
    assert!(
        invalid[0]
            .flags
            .contains(&yc_language::EntryFlag::PlaceholderError)
    );

    let valid = update_entries_batch_core(
        &opened,
        vec![EntryUpdate {
            id: entry.id,
            target_text: "$(item) 開始".to_owned(),
            origin: UpdateOrigin::Manual,
        }],
    )
    .expect("corrected Patchouli placeholder should clear its error");
    assert!(
        !valid[0]
            .flags
            .contains(&yc_language::EntryFlag::PlaceholderError)
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
