import { type ClassValue, clsx } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

export function formatTime(micros: number | null | undefined): string {
  if (!micros) return "-";
  return new Date(micros / 1000).toLocaleString(undefined, {
    year: "numeric",
    month: "short",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

/** A fire time as a person reads a calendar: "Mon 14 Sep, 09:00". */
export function formatFire(micros: number | null | undefined): string {
  if (!micros) return "-";
  return new Date(micros / 1000).toLocaleString(undefined, {
    weekday: "short",
    day: "numeric",
    month: "short",
    hour: "2-digit",
    minute: "2-digit",
  });
}

/** How far ahead a time is, in its two largest units: "in 1 d 21 h", "in 40 min". */
export function formatIn(micros: number): string {
  const ms = micros / 1000 - Date.now();
  if (ms <= 0) return "due";
  const minutes = Math.round(ms / 60_000);
  if (minutes < 1) return `in ${Math.max(1, Math.round(ms / 1000))} s`;
  if (minutes < 60) return `in ${minutes} min`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return minutes % 60 ? `in ${hours} h ${minutes % 60} min` : `in ${hours} h`;
  const days = Math.floor(hours / 24);
  return hours % 24 ? `in ${days} d ${hours % 24} h` : `in ${days} d`;
}

/** When something happened, briefly: "12 Sep, 11:02". */
export function formatStamp(micros: number): string {
  return new Date(micros / 1000).toLocaleString(undefined, {
    day: "numeric",
    month: "short",
    hour: "2-digit",
    minute: "2-digit",
  });
}

/** A time of day: "09:00". */
export function formatClock(micros: number): string {
  return new Date(micros / 1000).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
}

export function formatDuration(micros: number | null | undefined): string {
  if (micros === null || micros === undefined) return "-";
  const seconds = micros / 1_000_000;
  if (seconds < 1) return `${Math.round(seconds * 1000)} ms`;
  if (seconds < 60) return `${seconds.toFixed(2)} s`;
  const minutes = Math.floor(seconds / 60);
  const rest = Math.round(seconds - minutes * 60);
  if (minutes < 60) return `${minutes} min ${rest} s`;
  const hours = Math.floor(minutes / 60);
  return `${hours} h ${minutes - hours * 60} min`;
}

export function relativeTime(micros: number | null | undefined): string {
  if (!micros) return "-";
  const diff = Date.now() - micros / 1000;
  const abs = Math.abs(diff);
  const suffix = diff >= 0 ? "ago" : "from now";
  if (abs < 60_000) return `${Math.round(abs / 1000)} s ${suffix}`;
  if (abs < 3_600_000) return `${Math.round(abs / 60_000)} min ${suffix}`;
  if (abs < 86_400_000) return `${Math.round(abs / 3_600_000)} h ${suffix}`;
  return `${Math.round(abs / 86_400_000)} d ${suffix}`;
}

export const LEVEL_NAMES: Record<number, string> = {
  10: "DEBUG",
  20: "INFO",
  30: "WARNING",
  40: "ERROR",
  50: "CRITICAL",
};

export function levelName(level: number): string {
  return LEVEL_NAMES[level] ?? String(level);
}
