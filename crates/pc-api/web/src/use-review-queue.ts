import { useEffect, useRef, useState } from "react";
import { post, useDebounce, useResource } from "./api";
import type { Family } from "./types";
import { t } from "./i18n";

export type Decision = "plan" | "keep" | "defer";
export interface ReviewGroup extends Family {
  review_state: Decision | "pending";
  review_token: string;
  exact: boolean;
  can_plan: boolean;
  review_reasons: string[];
}
interface QueuePage {
  groups: ReviewGroup[];
  total: number;
  offset: number;
  counts: Record<Decision | "pending", number>;
}
export interface Batch {
  token: string;
  groups: number;
  files: number;
  bytes: number;
}
interface Undo {
  windowStart: number;
  operation: number;
  cursor: number;
  queue: string;
  kind: string;
  search: string;
}

export function openReviewedPlan() {
  sessionStorage.setItem("pc-reviewed-plan", "true");
  location.hash = "plan";
}
export function useReviewQueue(
  revision: number,
  disabled: boolean,
  onChange: () => void,
) {
  const [search, setSearch] = useState(""),
    [kind, setKind] = useState("all"),
    [queue, setQueue] = useState("pending"),
    [cursor, setCursor] = useState(0),
    [windowStart, setWindowStart] = useState(0),
    [history, setHistory] = useState<Undo[]>([]),
    [error, setError] = useState(""),
    [note, setNote] = useState(""),
    [busy, setBusy] = useState(false),
    [refreshing, setRefreshing] = useState(false);
  const lock = useRef(false);
  const query = useDebounce(search, 200),
    offset = Math.floor(cursor / 50) * 50;
  const path = `/review?${new URLSearchParams({ search: query, kind, queue, offset: String(offset), limit: "50" })}`;
  const r = useResource<QueuePage>(path, revision);
  const groups = r.data?.groups || [],
    total = r.data?.total || 0;
  const at = Math.max(
    0,
    Math.min(groups.length - 1, cursor - (r.data?.offset || 0)),
  );
  const current = groups[at];
  // Selection and viewport are separate: clicking a visible card never moves it.
  const start = Math.max(
    0,
    Math.min(
      at < windowStart ? at : at >= windowStart + 3 ? at - 2 : windowStart,
      Math.max(0, groups.length - 3),
    ),
  );
  useEffect(() => setWindowStart(start), [start]);
  useEffect(() => {
    if (r.data) {
      setRefreshing(false);
      setCursor((c) => Math.min(c, Math.max(0, r.data!.total - 1)));
    }
  }, [r.data]);
  useEffect(() => {
    if (r.error) setRefreshing(false);
  }, [r.error]);
  const blocked =
    disabled ||
    busy ||
    refreshing ||
    r.loading ||
    query !== search ||
    !!r.error;
  const filter = (value: string, type: "search" | "queue" | "kind") => {
    if (lock.current) return;
    ({ search: setSearch, queue: setQueue, kind: setKind })[type](value);
    setCursor(0);
    setWindowStart(0);
    setError("");
  };
  const move = (by: number) => {
    if (!blocked) setCursor(Math.max(0, Math.min(total - 1, cursor + by)));
  };
  const mutate = async (path: string, body: unknown, undoing = false) => {
    if (lock.current || blocked) return;
    lock.current = true;
    setBusy(true);
    setError("");
    const previous = history.at(-1);
    try {
      const result = await post<{ operation: number }>(path, body);
      if (undoing && previous) {
        setHistory((h) => h.slice(0, -1));
        setWindowStart(previous.windowStart);
        setCursor(previous.cursor);
        setQueue(previous.queue);
        setKind(previous.kind);
        setSearch(previous.search);
        setNote(t("rq_undone"));
      } else {
        setHistory((h) => [
          ...h.slice(-99),
          {
            windowStart: start,
            operation: result.operation,
            cursor,
            queue,
            kind,
            search,
          },
        ]);
        if (queue === "all") setCursor(Math.min(total - 1, cursor + 1));
        setNote(t("rq_saved"));
      }
      setRefreshing(true);
      r.reload();
      onChange();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      lock.current = false;
      setBusy(false);
    }
  };
  const decide = (state: Decision) => {
    if (!current || blocked || current.review_state === state) return;
    if (state === "plan" && !current.can_plan) {
      setNote(t("rq_not_eligible"));
      return;
    }
    if (state === "plan") sessionStorage.setItem("pc-reviewed-plan", "true");
    return mutate(`/review/${current.id}`, {
      state,
      token: current.review_token,
    });
  };
  return {
    r,
    search,
    kind,
    queue,
    cursor,
    start,
    at,
    current,
    groups,
    total,
    history,
    error,
    note,
    blocked,
    busy,
    filter,
    move,
    select: (at: number) => {
      if (!blocked) setCursor((r.data?.offset || 0) + at);
    },
    jump: (at: number) => {
      if (!blocked && Number.isFinite(at))
        setCursor(Math.max(0, Math.min(total - 1, at)));
    },
    decide,
    undo: () =>
      history.length &&
      mutate("/review/undo", { operation: history.at(-1)!.operation }, true),
    applyBatch: (batch: Batch) =>
      mutate("/review/batch", { token: batch.token }),
  };
}
