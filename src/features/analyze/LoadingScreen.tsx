import { useProjectStore } from "../../stores/projectStore";
import { useT } from "../../i18n";

export function LoadingScreen() {
  const title = useProjectStore((state) => state.operationTitle);
  const progress = useProjectStore((state) => state.operationProgress);
  const translate = useT();

  return (
    <section className="focus-page analyze-screen" aria-labelledby="operation-title" data-testid="project-loading">
      <div className="analyze-screen__content">
        <h1 className="page-title" id="operation-title">{title || translate("common.loading")}</h1>
        {/* 不確定進捗を共通ProgressBarの契約に持ち込まず、この処理画面で表現する。 */}
        <progress
          className="progress-bar"
          max={100}
          {...(progress?.percent == null ? {} : { value: progress.percent })}
          aria-label={translate("analyze.loadingProgress")}
        />
        <p className="analyze-screen__stage" aria-live="polite">
          {progress?.message ?? translate("common.preparing")}
        </p>
      </div>
    </section>
  );
}
