import { cloneElement, useId, useLayoutEffect, useRef, useState } from "react";
import type { ReactElement } from "react";
import { createPortal } from "react-dom";

interface TooltipAnchor {
  top: number;
  bottom: number;
  left: number;
  width: number;
}

interface TooltipSize {
  width: number;
  height: number;
}

export interface TooltipPosition {
  top: number;
  left: number;
  placement: "above" | "below";
}

export function calculateTooltipPosition(
  anchor: TooltipAnchor,
  tooltip: TooltipSize,
  viewport: { width: number; height: number },
  padding = 8,
): TooltipPosition {
  const gap = 8;
  const placement = anchor.top - tooltip.height - gap >= padding ? "above" : "below";
  const wantedTop = placement === "above" ? anchor.top - tooltip.height - gap : anchor.bottom + gap;
  const maxTop = Math.max(padding, viewport.height - tooltip.height - padding);
  const maxLeft = Math.max(padding, viewport.width - tooltip.width - padding);
  const centeredLeft = anchor.left + (anchor.width - tooltip.width) / 2;

  return {
    top: Math.min(Math.max(padding, wantedTop), maxTop),
    left: Math.min(Math.max(padding, centeredLeft), maxLeft),
    placement,
  };
}

interface TooltipProps {
  children: ReactElement<{ "aria-describedby"?: string }>;
  content: string;
  shortcut?: string;
}

export function Tooltip({ children, content, shortcut }: TooltipProps) {
  const id = useId();
  const wrapperRef = useRef<HTMLSpanElement>(null);
  const tooltipRef = useRef<HTMLSpanElement>(null);
  const [visible, setVisible] = useState(false);
  const [position, setPosition] = useState<TooltipPosition | null>(null);
  const previousDescription = children.props["aria-describedby"];
  const describedBy = [previousDescription, id].filter(Boolean).join(" ");
  const describedChild = cloneElement(children, { "aria-describedby": describedBy });

  useLayoutEffect(() => {
    if (!visible) return;

    const updatePosition = () => {
      const anchor = wrapperRef.current?.getBoundingClientRect();
      const tooltip = tooltipRef.current?.getBoundingClientRect();
      if (!anchor || !tooltip) return;
      setPosition(calculateTooltipPosition(
        { top: anchor.top, bottom: anchor.bottom, left: anchor.left, width: anchor.width },
        { width: tooltip.width, height: tooltip.height },
        { width: window.innerWidth, height: window.innerHeight },
      ));
    };

    updatePosition();
    window.addEventListener("resize", updatePosition);
    document.addEventListener("scroll", updatePosition, true);
    return () => {
      window.removeEventListener("resize", updatePosition);
      document.removeEventListener("scroll", updatePosition, true);
    };
  }, [visible, content, shortcut]);

  return (
    <span
      className="tooltip"
      data-visible={visible || undefined}
      ref={wrapperRef}
      onMouseEnter={() => setVisible(true)}
      onMouseLeave={() => {
        if (!wrapperRef.current?.contains(document.activeElement)) setVisible(false);
      }}
      onFocusCapture={() => setVisible(true)}
      onBlurCapture={(event) => {
        if (!wrapperRef.current?.contains(event.relatedTarget as Node | null)) setVisible(false);
      }}
    >
      {describedChild}
      {typeof document !== "undefined" && createPortal(
        <span
          className="tooltip__content"
          id={id}
          ref={tooltipRef}
          role="tooltip"
          data-visible={visible || undefined}
          data-positioned={position ? "true" : "false"}
          data-placement={position?.placement}
          style={{ top: position?.top ?? 0, left: position?.left ?? 0 }}
        >
          <span>{content}</span>
          {shortcut && <kbd>{shortcut}</kbd>}
        </span>,
        document.body,
      )}
    </span>
  );
}
