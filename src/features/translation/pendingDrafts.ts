type DraftCommit = () => void;

const draftCommits = new Set<DraftCommit>();

export function registerPendingDraftCommit(commit: DraftCommit) {
  draftCommits.add(commit);
  return () => {
    draftCommits.delete(commit);
  };
}

export function commitPendingDrafts() {
  for (const commit of draftCommits) commit();
}
