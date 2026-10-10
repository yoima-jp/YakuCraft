interface ProgressBarProps {
  value: number;
  label: string;
}

export function ProgressBar({ value, label }: ProgressBarProps) {
  return <progress className="progress-bar" max={100} value={value} aria-label={label} />;
}
