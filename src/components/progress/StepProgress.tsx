import { ProgressBar } from "./ProgressBar";
import { formatNumber, useT } from "../../i18n";

interface StepProgressProps {
  step: number;
  total: number;
}

export function StepProgress({ step, total }: StepProgressProps) {
  const translate = useT();
  const value = (step / total) * 100;

  return (
    <div className="step-progress">
      <ProgressBar value={value} label={translate("common.setupProgress")} />
      <span className="step-progress__count">{formatNumber(step)} / {formatNumber(total)}</span>
    </div>
  );
}
