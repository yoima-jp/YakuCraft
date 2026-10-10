import { ProgressBar } from "./ProgressBar";

interface StepProgressProps {
  step: number;
  total: number;
}

export function StepProgress({ step, total }: StepProgressProps) {
  const value = (step / total) * 100;

  return (
    <div className="step-progress">
      <ProgressBar value={value} label="セットアップの進行状況" />
      <span className="step-progress__count">{step} / {total}</span>
    </div>
  );
}
