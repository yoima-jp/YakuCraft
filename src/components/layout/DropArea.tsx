import type { ReactNode } from "react";

interface DropAreaProps {
  isDragging: boolean;
  children: ReactNode;
}

export function DropArea({ isDragging, children }: DropAreaProps) {
  const className = isDragging
    ? "drop-area drop-area--active"
    : "drop-area";

  return (
    <section className={className} aria-label="JAR・シェーダーパックのドロップ領域">
      <svg className="drop-area__icon" viewBox="0 0 48 48" aria-hidden="true">
        <path d="M24 5v23m-9-9 9 9 9-9M8 31v8a4 4 0 0 0 4 4h24a4 4 0 0 0 4-4v-8" />
      </svg>
      {children}
    </section>
  );
}
