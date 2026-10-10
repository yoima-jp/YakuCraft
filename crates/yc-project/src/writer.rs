use std::{
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender, SyncSender},
    thread::{self, JoinHandle},
};

use yc_language::{EntryFlag, ReviewSettings};

use crate::{
    AnalysisData, EntryUpdate, GlossaryTermInput, JarReplacementRecord, ProjectAssetIndex,
    ProjectError, Result, database::open_database, repository,
};

enum WriterCommand {
    SaveAnalysis(i64, AnalysisData, SyncSender<Result<()>>),
    SaveAssetIndex(i64, ProjectAssetIndex, SyncSender<Result<()>>),
    SaveReviewSettings(i64, ReviewSettings, bool, SyncSender<Result<()>>),
    RemoveModUnit(i64, SyncSender<Result<()>>),
    UpdateEntries(Vec<EntryUpdate>, SyncSender<Result<()>>),
    AddFlag(i64, EntryFlag, SyncSender<Result<()>>),
    RemoveFlag(i64, EntryFlag, SyncSender<Result<()>>),
    SaveJarReplacement(JarReplacementRecord, SyncSender<Result<()>>),
    MarkJarReplacementRestored(i64, String, SyncSender<Result<()>>),
    UpsertGlossaryTerms(Vec<GlossaryTermInput>, SyncSender<Result<()>>),
    UpdateGlossaryTerm(i64, GlossaryTermInput, SyncSender<Result<()>>),
    DeleteGlossaryTerm(i64, SyncSender<Result<()>>),
    Stop,
}

pub struct ProjectWriter {
    sender: Sender<WriterCommand>,
    thread: Option<JoinHandle<()>>,
}

impl ProjectWriter {
    pub(crate) fn start(database_path: impl AsRef<Path>) -> Result<Self> {
        let database_path = database_path.as_ref().to_path_buf();
        let (sender, receiver) = mpsc::channel();
        let (started, startup_result) = mpsc::sync_channel(1);
        let writer_thread = database_path.clone();
        let thread = thread::Builder::new()
            .name("yc-project-writer".to_owned())
            .spawn(move || writer_loop(writer_thread, receiver, started))?;
        match startup_result.recv() {
            Ok(Ok(())) => Ok(Self {
                sender,
                thread: Some(thread),
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(ProjectError::WriterStartup(error))
            }
            Err(_) => {
                let _ = thread.join();
                Err(ProjectError::WriterStopped)
            }
        }
    }

    pub fn save_analysis(&self, mod_unit_id: i64, analysis: AnalysisData) -> Result<()> {
        self.send_and_wait(|response| WriterCommand::SaveAnalysis(mod_unit_id, analysis, response))
    }

    pub fn save_asset_index(&self, mod_unit_id: i64, index: ProjectAssetIndex) -> Result<()> {
        self.send_and_wait(|response| WriterCommand::SaveAssetIndex(mod_unit_id, index, response))
    }

    pub fn save_review_settings(
        &self,
        mod_unit_id: i64,
        settings: ReviewSettings,
        setup_completed: bool,
    ) -> Result<()> {
        self.send_and_wait(|response| {
            WriterCommand::SaveReviewSettings(mod_unit_id, settings, setup_completed, response)
        })
    }

    pub fn remove_mod_unit(&self, mod_unit_id: i64) -> Result<()> {
        self.send_and_wait(|response| WriterCommand::RemoveModUnit(mod_unit_id, response))
    }

    pub fn update_entry(&self, update: EntryUpdate) -> Result<()> {
        self.update_entries(vec![update])
    }

    pub fn update_entries(&self, updates: Vec<EntryUpdate>) -> Result<()> {
        self.send_and_wait(|response| WriterCommand::UpdateEntries(updates, response))
    }

    pub fn add_flag(&self, entry_id: i64, flag: EntryFlag) -> Result<()> {
        self.send_and_wait(|response| WriterCommand::AddFlag(entry_id, flag, response))
    }

    pub fn remove_flag(&self, entry_id: i64, flag: EntryFlag) -> Result<()> {
        self.send_and_wait(|response| WriterCommand::RemoveFlag(entry_id, flag, response))
    }

    pub fn save_jar_replacement(&self, replacement: JarReplacementRecord) -> Result<()> {
        self.send_and_wait(|response| WriterCommand::SaveJarReplacement(replacement, response))
    }

    pub fn mark_jar_replacement_restored(&self, id: i64, restored_at: String) -> Result<()> {
        self.send_and_wait(|response| {
            WriterCommand::MarkJarReplacementRestored(id, restored_at, response)
        })
    }

    pub fn upsert_glossary_term(&self, term: GlossaryTermInput) -> Result<()> {
        self.upsert_glossary_terms(vec![term])
    }

    pub fn upsert_glossary_terms(&self, terms: Vec<GlossaryTermInput>) -> Result<()> {
        self.send_and_wait(|response| WriterCommand::UpsertGlossaryTerms(terms, response))
    }

    pub fn update_glossary_term(&self, id: i64, term: GlossaryTermInput) -> Result<()> {
        self.send_and_wait(|response| WriterCommand::UpdateGlossaryTerm(id, term, response))
    }

    pub fn delete_glossary_term(&self, id: i64) -> Result<()> {
        self.send_and_wait(|response| WriterCommand::DeleteGlossaryTerm(id, response))
    }

    fn send_and_wait(
        &self,
        command: impl FnOnce(SyncSender<Result<()>>) -> WriterCommand,
    ) -> Result<()> {
        let (response, result) = mpsc::sync_channel(1);
        self.sender
            .send(command(response))
            .map_err(|_| ProjectError::WriterStopped)?;
        result.recv().map_err(|_| ProjectError::WriterStopped)?
    }
}

impl Drop for ProjectWriter {
    fn drop(&mut self) {
        let _ = self.sender.send(WriterCommand::Stop);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("Project writer thread panicked");
        }
    }
}

fn writer_loop(
    database_path: PathBuf,
    commands: Receiver<WriterCommand>,
    started: SyncSender<std::result::Result<(), String>>,
) {
    let mut connection = match open_database(&database_path) {
        Ok(connection) => {
            if started.send(Ok(())).is_err() {
                return;
            }
            connection
        }
        Err(error) => {
            let _ = started.send(Err(error.to_string()));
            return;
        }
    };

    while let Ok(command) = commands.recv() {
        match command {
            WriterCommand::SaveAnalysis(mod_unit_id, analysis, response) => {
                let _ = response.send(repository::save_analysis(
                    &mut connection,
                    mod_unit_id,
                    analysis,
                ));
            }
            WriterCommand::SaveAssetIndex(mod_unit_id, index, response) => {
                let _ = response.send(repository::save_asset_index(
                    &mut connection,
                    mod_unit_id,
                    index,
                ));
            }
            WriterCommand::SaveReviewSettings(mod_unit_id, settings, setup_completed, response) => {
                let _ = response.send(repository::save_review_settings(
                    &mut connection,
                    mod_unit_id,
                    settings,
                    setup_completed,
                ));
            }
            WriterCommand::RemoveModUnit(mod_unit_id, response) => {
                let _ = response.send(repository::remove_mod_unit(
                    &mut connection,
                    mod_unit_id,
                    false,
                ));
            }
            WriterCommand::UpdateEntries(updates, response) => {
                let _ = response.send(repository::update_entries(&mut connection, updates));
            }
            WriterCommand::AddFlag(entry_id, flag, response) => {
                let _ = response.send(repository::add_flag(&mut connection, entry_id, flag));
            }
            WriterCommand::RemoveFlag(entry_id, flag, response) => {
                let _ = response.send(repository::remove_flag(&mut connection, entry_id, flag));
            }
            WriterCommand::SaveJarReplacement(replacement, response) => {
                let _ = response.send(repository::save_jar_replacement(
                    &mut connection,
                    replacement,
                ));
            }
            WriterCommand::MarkJarReplacementRestored(id, restored_at, response) => {
                let _ = response.send(repository::mark_jar_replacement_restored(
                    &mut connection,
                    id,
                    &restored_at,
                ));
            }
            WriterCommand::UpsertGlossaryTerms(terms, response) => {
                let _ = response.send(repository::upsert_glossary_terms(&mut connection, terms));
            }
            WriterCommand::UpdateGlossaryTerm(id, term, response) => {
                let _ = response.send(repository::update_glossary_term(&mut connection, id, term));
            }
            WriterCommand::DeleteGlossaryTerm(id, response) => {
                let _ = response.send(repository::delete_glossary_term(&mut connection, id));
            }
            WriterCommand::Stop => break,
        }
    }
}
