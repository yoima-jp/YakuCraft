import { PrimaryButton } from "../../components/button/Buttons";
import { installAvailableUpdate, useUpdaterStore } from "./updater";

export function UpdaterNotice() {
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
          <span aria-live="polite">更新をダウンロードしています</span>
          <progress
            aria-label="更新のダウンロード"
            max={contentLength ?? undefined}
            value={contentLength ? downloadedBytes : undefined}
          />
          {progress !== null && <span aria-hidden="true">{progress}%</span>}
        </div>
      ) : (
        <>
          <output aria-live="polite">新しいバージョン {update.version} があります</output>
          <PrimaryButton onClick={() => void installAvailableUpdate()}>更新</PrimaryButton>
        </>
      )}
    </aside>
  );
}
