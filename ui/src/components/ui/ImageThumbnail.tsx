import { useEffect, useState, type ComponentType } from "react";
import { Image as ImageIcon } from "lucide-react";
import { ICON_STROKE } from "@/design/tokens";
import { cn } from "@/lib/utils";

/**
 * Renders an image thumbnail from `src` and falls back to an icon when the
 * image fails to load (for example under browser dev mode without Tauri).
 */
export function ImageThumbnail({
  src,
  alt,
  className,
  fallbackClassName,
  icon: Icon = ImageIcon,
  loading,
}: {
  src: string;
  alt: string;
  className?: string;
  fallbackClassName?: string;
  icon?: ComponentType<{ className?: string; strokeWidth?: number; "aria-hidden"?: boolean }>;
  loading?: "eager" | "lazy";
}) {
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    setFailed(false);
  }, [src]);

  if (!src || failed) {
    return (
      <div
        className={cn("flex h-full w-full items-center justify-center text-fg3", fallbackClassName)}
      >
        <Icon className="size-6 shrink-0" strokeWidth={ICON_STROKE} aria-hidden />
      </div>
    );
  }

  return (
    <img
      src={src}
      alt={alt}
      loading={loading}
      onError={() => setFailed(true)}
      className={className}
    />
  );
}
