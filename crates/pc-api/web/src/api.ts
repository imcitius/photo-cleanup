import { useEffect, useRef, useState } from "react";
export async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
  const res = await fetch(`/api${path}`, {
    ...init,
    headers: { "Content-Type": "application/json", ...init.headers },
  });
  const text = await res.text();
  let body;
  try {
    body = JSON.parse(text);
  } catch {
    body = { error: text || res.statusText };
  }
  if (!res.ok) throw new Error(`${body.error || res.statusText} (${path})`);
  return body as T;
}
export const post = <T>(path: string, body: unknown = {}) =>
  api<T>(path, { method: "POST", body: JSON.stringify(body) });
export function useResource<T>(path: string | null, revision = 0) {
  const [data, setData] = useState<T | null>(null),
    [error, setError] = useState(""),
    [loading, setLoading] = useState(true),
    [nonce, setNonce] = useState(0);
  const lastPath = useRef(path);
  useEffect(() => {
    const controller = new AbortController();
    if (lastPath.current !== path || !path) setData(null);
    lastPath.current = path;
    setError("");
    setLoading(!!path);
    if (path)
      api<T>(path, { signal: controller.signal })
        .then((value) => {
          if (!controller.signal.aborted) setData(value);
        })
        .catch((e) => {
          if (e.name !== "AbortError") setError(e.message);
        })
        .finally(() => {
          if (!controller.signal.aborted) setLoading(false);
        });
    return () => controller.abort();
  }, [path, revision, nonce]);
  return {
    data,
    setData,
    error,
    loading,
    reload: () => setNonce((n) => n + 1),
  };
}
export function useDebounce<T>(value: T, ms = 250) {
  const [result, setResult] = useState(value);
  useEffect(() => {
    const timer = setTimeout(() => setResult(value), ms);
    return () => clearTimeout(timer);
  }, [value, ms]);
  return result;
}
export function useLatest<T>(value: T) {
  const ref = useRef(value);
  ref.current = value;
  return ref;
}
