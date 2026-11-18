import { Children, type ComponentPropsWithoutRef, type ReactNode } from "react";
import { cn } from "@/lib/utils";

/** A bordered card of setting rows, separated by hairlines. */
export function SettingsCard({ children }: { children: ReactNode }) {
  return (
    <div className="overflow-hidden rounded-xl border border-hair bg-surface">
      {Children.toArray(children).map((child, idx) => (
        <div key={idx} className={cn(idx > 0 && "border-t border-hair-soft")}>
          {child}
        </div>
      ))}
    </div>
  );
}

/**
 * One setting: label and optional description on the left, the control on the
 * right. `labelId` lets the control name itself with `aria-labelledby`.
 */
export function SettingRow({
  labelId,
  label,
  description,
  children,
}: {
  labelId: string;
  label: string;
  description?: string;
  children: ReactNode;
}) {
  return (
    <div className="flex items-center justify-between gap-4 px-5 py-3.5">
      <div className="min-w-0 flex-1">
        <div id={labelId} className="text-body-sm font-medium text-fg">
          {label}
        </div>
        {description && (
          <div className="mt-0.5 text-caption leading-relaxed text-fg2">{description}</div>
        )}
      </div>
      <div className="shrink-0">{children}</div>
    </div>
  );
}

/** A read-only value chip, shaped like the select controls beside it. */
export function ValueChip({ children, className, ...props }: ComponentPropsWithoutRef<"span">) {
  return (
    <span
      className={cn(
        "inline-flex h-8 select-text items-center rounded-md border border-hair bg-app px-2.5 text-[13px] font-medium text-fg2",
        className,
      )}
      {...props}
    >
      {children}
    </span>
  );
}
