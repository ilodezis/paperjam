import { useLayoutEffect, useRef, useState, type ComponentType, type ReactNode } from "react";

export interface SegmentedOption<T extends string> {
  value: T;
  label: ReactNode;
  icon?: ComponentType<{ className?: string; "aria-hidden"?: boolean | "true" | "false" }>;
  ariaLabel?: string;
}

export interface SegmentedControlProps<T extends string> {
  value: T;
  onChange: (value: T) => void;
  options: SegmentedOption<T>[];
  ariaLabel?: string;
  className?: string;
  name?: string;
  id?: string;
}

/** Where the thumb sits: the chosen pill's offset and width inside the track. */
interface Thumb {
  x: number;
  width: number;
  /** The first placement is not animated: the thumb appears under the chosen pill. */
  instant: boolean;
}

export function SegmentedControl<T extends string>({
  value,
  onChange,
  options,
  ariaLabel,
  className = "",
  name,
  id,
}: SegmentedControlProps<T>) {
  const track = useRef<HTMLDivElement>(null);
  const [thumb, setThumb] = useState<Thumb | null>(null);

  useLayoutEffect(() => {
    const node = track.current;
    if (!node) return;
    function place() {
      const active = node?.querySelector<HTMLElement>(".segmented-pill--active");
      if (!active || active.offsetWidth === 0) return;
      setThumb((current) => ({ x: active.offsetLeft, width: active.offsetWidth, instant: current === null }));
    }
    place();
    // A hidden tab panel measures as zero; place the thumb once it is shown or resized.
    if (typeof ResizeObserver !== "function") return;
    const observer = new ResizeObserver(place);
    observer.observe(node);
    return () => observer.disconnect();
  }, [value, options.length]);

  return (
    <div
      ref={track}
      className={`segmented-control ${thumb ? "segmented-control--measured" : ""} ${className}`}
      role="radiogroup"
      id={id}
    >
      {/* Visually hidden native select for full form/test compatibility */}
      <select
        className="sr-only"
        aria-label={ariaLabel}
        name={name}
        value={value}
        onChange={(event) => onChange(event.target.value as T)}
        tabIndex={-1}
      >
        {options.map((option) => (
          <option key={option.value} value={option.value}>
            {typeof option.label === "string" ? option.label : option.value}
          </option>
        ))}
      </select>

      {thumb ? (
        <span
          className={`segmented-thumb ${thumb.instant ? "segmented-thumb--instant" : ""}`}
          style={{ width: thumb.width, transform: `translateX(${thumb.x}px)` }}
          aria-hidden="true"
        />
      ) : null}

      {options.map((option) => {
        const active = option.value === value;
        const Icon = option.icon;
        return (
          <button
            key={option.value}
            type="button"
            role="radio"
            aria-checked={active}
            aria-label={option.ariaLabel ?? (typeof option.label === "string" ? option.label : undefined)}
            className={`segmented-pill ${active ? "segmented-pill--active" : ""}`}
            onClick={() => onChange(option.value)}
          >
            {Icon ? <Icon aria-hidden="true" className="segmented-icon" /> : null}
            <span className="segmented-label">{option.label}</span>
          </button>
        );
      })}
    </div>
  );
}
