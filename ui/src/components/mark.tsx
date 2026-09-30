import { useEffect, useState } from "react";
import { cn } from "@/lib/utils";

export function useTheme() {
  const [dark, setDark] = useState(() => document.documentElement.classList.contains("dark"));
  useEffect(() => {
    document.documentElement.classList.toggle("dark", dark);
    try {
      localStorage.setItem("cereyan-theme", dark ? "dark" : "light");
    } catch {}
  }, [dark]);
  return { dark, toggle: () => setDark((d) => !d) };
}

/**
 * The brand mark: an ink tile with one full wave of alternating current, the
 * sign for the word cereyan. The same path draws `ui/public/favicon.svg` and
 * the docs' `docs/assets/logo.svg` and `favicon.svg`; change all four together.
 */
export function Mark({ className }: { className?: string }) {
  return (
    <svg width="18" height="18" viewBox="0 0 18 18" className={cn("shrink-0", className)} aria-hidden="true">
      <rect width="18" height="18" rx="5" className="fill-primary" />
      <path
        d="M3.5 9C5.3 4 7.2 4 9 9S12.7 14 14.5 9"
        className="stroke-primary-foreground"
        strokeWidth="1.6"
        fill="none"
        strokeLinecap="round"
      />
    </svg>
  );
}
