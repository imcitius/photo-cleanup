// Which language the interface speaks, and the formatting that follows from
// it. The dictionaries themselves live in ./locales.
//
// The language is read once, before the first render, and a change reloads
// the page. Threading a live locale through every component would buy the
// ability to switch without a reload — which nobody does twice in a session
// — at the cost of touching every string in the interface.
import { ui as ruUi, messages as ruMessages } from "./locales/ru";
import { ui as enUi, messages as enMessages } from "./locales/en";

export type Language = "ru" | "en";
export const LANGUAGES: Language[] = ["ru", "en"];

const KEY = "pc-lang";

function detect(): Language {
  try {
    const saved = localStorage.getItem(KEY);
    if (saved === "ru" || saved === "en") return saved;
  } catch {
    // Private windows and blocked site data: fall through to the browser's.
  }
  return navigator.language?.toLowerCase().startsWith("ru") ? "ru" : "en";
}

export const language: Language = detect();

/** Remember the choice and start again in it. */
export function setLanguage(next: Language) {
  try {
    localStorage.setItem(KEY, next);
  } catch {
    // Not remembering it is better than refusing to switch.
  }
  if (next !== language) location.reload();
}

const dictionaries = {
  ru: { ui: ruUi, messages: ruMessages, tag: "ru-RU" },
  en: { ui: enUi, messages: enMessages, tag: "en-GB" },
} as const;

const active = dictionaries[language];

export const ui = active.ui;

// Screen readers and the browser's own chrome read these, so they follow the
// locale like everything else.
document.documentElement.lang = language;
document.title = active.ui.title;
const messages = active.messages;
const tag = active.tag;

type MessageKey = keyof typeof ruMessages;

export function t(key: MessageKey, ...values: unknown[]): string {
  // An untranslated key falls back to Russian rather than to a blank space.
  const text: string =
    (messages as Partial<Record<MessageKey, string>>)[key] ?? ruMessages[key];
  return text.replace(/\{(\d+)\}/g, (_, n) => String(values[Number(n)] ?? ""));
}

/** What a role means, in one sentence. The badge alone says too little. */
export const roleHelp = (role: string) =>
  ui.roleHelp[role as keyof typeof ui.roleHelp] || "";

export const jobName = (kind: string) =>
  ui.jobs[kind as keyof typeof ui.jobs] || kind;
export const stateName = (state: string) =>
  ui.states[state as keyof typeof ui.states] || state;
export const dateSourceName = (source: string) =>
  ui.dateSources[source as keyof typeof ui.dateSources] || source;

export const number = (n: number) => n.toLocaleString(tag);

export function bytes(n: number) {
  let i = 0;
  while (n >= 1024 && i < 4) {
    n /= 1024;
    i++;
  }
  return `${n.toLocaleString(tag, { maximumFractionDigits: i ? 1 : 0 })} ${ui.units[i]}`;
}

export const when = (n: number | null | undefined) =>
  n == null
    ? ui.unknownDate
    : new Date(n * 1000).toLocaleString(tag, {
        timeZone: "UTC",
        year: "numeric",
        month: "2-digit",
        day: "2-digit",
        hour: "2-digit",
        minute: "2-digit",
      });

/** The date alone, for places where the time would only take up room. */
export const day = (n: number | null | undefined) =>
  n == null
    ? ui.unknownDate
    : new Date(n * 1000).toLocaleDateString(tag, {
        timeZone: "UTC",
        year: "numeric",
        month: "2-digit",
        day: "2-digit",
      });

/** Seconds as a person says them: "3 мин 20 с", "1 h 12 min". */
export function duration(seconds: number) {
  const s = Math.max(0, Math.round(seconds));
  if (s < 60) return `${s} ${ui.secondsShort}`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m} ${ui.minutesShort} ${s % 60} ${ui.secondsShort}`;
  return `${Math.floor(m / 60)} ${ui.hoursShort} ${m % 60} ${ui.minutesShort}`;
}

/** A drive letter or a UNC prefix: the only paths that use backslashes. */
const WINDOWS_PATH = /^(?:[A-Za-z]:[\\/]|\\\\)/;

/**
 * The last component of a path, as the server writes it.
 *
 * A Windows server sends backslashes; a Unix one sends slashes, and there a
 * backslash is an ordinary character in a file name — `a\b.jpg` is one file.
 * So the backslash counts as a separator only for a path that announces
 * itself as a Windows one.
 */
export const basename = (p: string) =>
  (WINDOWS_PATH.test(p) ? p.split(/[/\\]/) : p.split("/")).pop() || p;

/** Colour for a finished job's badge. */
export const jobTone = (state: string) =>
  state === "failed" || state === "interrupted"
    ? "warning"
    : state === "cancelled"
      ? "muted"
      : "";

/**
 * True when the chosen folders look like an Unraid array: separate disks
 * mounted side by side, with a union view over them.
 *
 * The advice that follows from that — pick /mnt/diskN so a move stays on one
 * spindle, and the warning that a lost disk is a lost disk — is real, and
 * wrong to show to everyone else. Nobody indexing a laptop's Pictures folder
 * needs to read about somebody's NAS.
 */
export const looksLikeArray = (roots: string[] | undefined) =>
  !!roots?.some((r) => /^\/mnt\/(disk\d+|user|cache)\b/.test(r));
