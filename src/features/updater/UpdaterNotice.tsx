import { PrimaryButton } from "../../components/button/Buttons";
import { useT } from "../../i18n";
import { installAvailableUpdate, useUpdaterStore } from "./updater";

export function UpdaterNotice() {
  const translate = useT();
  const update = useUpdaterStore((state) => state.availableUpdate);
  const installing = useUpdaterStore((state) => state.installing);
  const downloadedBytes = useUpdaterStore((state) => state.downloadedBytes);
  const contentLength = useUpdaterStore((state) => state.contentLength);

  if (!update) return null;

  const progress = contentLength && contentLength > 0
    ? Math.min(100, Math.round((downloadedBytes / contentLength) * 100))
    : null;

  return (
    <aside className="home-screen__update">
      {installing ? (
        <div className="home-screen__update-progress">
          <span aria-live="polite">{translate("updater.downloading")}</span>
          <progress
            aria-label={translate("updater.downloadAria")}
            max={contentLength ?? undefined}
            value={contentLength ? downloadedBytes : undefined}
          />
          {progress !== null && <span aria-hidden="true">{progress}%</span>}
        </div>
      ) : (
        <>
          <output aria-live="polite">{translate("updater.versionAvailable", { version: update.version })}</output>
          <PrimaryButton onClick={() => void installAvailableUpdate()}>{translate("updater.update")}</PrimaryButton>
        </>
      )}
    </aside>
  );
}
