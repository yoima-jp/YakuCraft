import { useProjectStore } from "../../stores/projectStore";

export function LoadingScreen() {
  const title = useProjectStore((state) => state.operationTitle);
  const progress = useProjectStore((state) => state.operationProgress);

  return (
    <section className="focus-page analyze-screen" aria-labelledby="operation-title" data-testid="project-loading">
      <div className="analyze-screen__content">
        <h1 className="page-title" id="operation-title">{title}</h1>
        {/* 不確定進捗を共通ProgressBarの契約に持ち込まず、この処理画面で表現する。 */}
        <progress
          className="progress-bar"
          max={100}
          {...(progress?.percent == null ? {} : { value: progress.percent })}
          aria-label="処理の進行状況"
        />
        <p className="analyze-screen__stage" aria-live="polite">
          {progress?.message ?? "準備中…"}
        </p>
      </div>
    </section>
  );
}
