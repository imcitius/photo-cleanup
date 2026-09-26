import { t } from "./i18n";
import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import { dateSourceName, ui, bytes, number, when } from "./i18n";
import { useResource } from "./api";
import { isDesktop, pickFolder } from "./desktop";
import type { FileDetails as FileDetailsRow } from "./types";
export function Icon({
  name = "grid",
  size = 20,
}: {
  name?: string;
  size?: number;
}) {
  const paths: Record<string, ReactNode> = {
    grid: (
      <>
        <rect x="3" y="3" width="7" height="7" rx="1.5" />
        <rect x="14" y="3" width="7" height="7" rx="1.5" />
        <rect x="3" y="14" width="7" height="7" rx="1.5" />
        <rect x="14" y="14" width="7" height="7" rx="1.5" />
      </>
    ),
    folder: (
      <path d="M3 7V5a2 2 0 0 1 2-2h5l2 3h7a2 2 0 0 1 2 2v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7Z" />
    ),
    layers: (
      <>
        <path d="m12 3 10 5-10 5L2 8Z" />
        <path d="m2 12 10 5 10-5M2 16l10 5 10-5" />
      </>
    ),
    image: (
      <>
        <rect x="3" y="3" width="18" height="18" rx="3" />
        <circle cx="8" cy="8" r="1.5" />
        <path d="m3 17 5-5 4 4 4-7 5 8" />
      </>
    ),
    series: (
      <>
        <rect x="7" y="5" width="14" height="16" rx="2" />
        <path d="M17 2H4a2 2 0 0 0-2 2v13M12 10l5 3-5 3Z" />
      </>
    ),
    shield: (
      <>
        <path d="m12 2 9 4v6c0 5-9 10-9 10S3 17 3 12V6Z" />
        <path d="m8 12 3 3 5-6" />
      </>
    ),
    archive: (
      <>
        <path d="M4 8v12h16V8M9 12h6" />
        <rect x="2" y="3" width="20" height="5" rx="1" />
      </>
    ),
    list: (
      <>
        <path d="M8 6h13M8 12h13M8 18h13M3 6h.01M3 12h.01M3 18h.01" />
      </>
    ),
    settings: (
      <>
        <circle cx="12" cy="12" r="3" />
        <path d="m9 3 1-1h4l1 3 3 1 3 3-1 3 1 3-3 3-3 1-1 3h-4l-1-3-3-1-3-3 1-3-1-3 3-3 3-1Z" />
      </>
    ),
    arrow: <path d="M4 12h16m-6-6 6 6-6 6" />,
    "arrow-left": <path d="M20 12H4m6-6-6 6 6 6" />,
    check: <path d="m5 12 4 4L19 6" />,
    close: <path d="m6 6 12 12M6 18 18 6" />,
    search: (
      <>
        <circle cx="10" cy="10" r="7" />
        <path d="m15 15 6 6" />
      </>
    ),
    plus: <path d="M12 5v14M5 12h14" />,
    clock: (
      <>
        <circle cx="12" cy="12" r="9" />
        <path d="M12 7v5l4 2" />
      </>
    ),
    alert: (
      <>
        <path d="M12 3 2 21h20Z" />
        <path d="M12 9v5m0 3h.01" />
      </>
    ),
    server: (
      <>
        <rect x="3" y="3" width="18" height="7" rx="2" />
        <rect x="3" y="14" width="18" height="7" rx="2" />
        <path d="M7 6.5h.01M7 17.5h.01M12 6.5h5M12 17.5h5" />
      </>
    ),
    sun: (
      <>
        <circle cx="12" cy="12" r="4" />
        <path d="M12 1v2m0 18v2M1 12h2m18 0h2M4 4l2 2m12 12 2 2M4 20l2-2M18 6l2-2" />
      </>
    ),
    trash: (
      <>
        <path d="M3 6h18M9 6V3h6v3M6 6l1 15h10l1-15M10 10v7m4-7v7" />
      </>
    ),
    download: (
      <>
        <path d="M12 3v12m-5-5 5 5 5-5M4 16v5h16v-5" />
      </>
    ),
  };
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.6"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      {paths[name] || paths.grid}
    </svg>
  );
}
export function Button({
  children,
  icon,
  kind = "",
  ...p
}: React.ButtonHTMLAttributes<HTMLButtonElement> & {
  icon?: string;
  kind?: string;
}) {
  return (
    <button
      {...p}
      onClick={(e) => {
        e.currentTarget.focus();
        p.onClick?.(e);
      }}
      className={`button ${kind} ${p.className || ""}`}
    >
      {icon && <Icon name={icon} size={16} />}
      <span>{children}</span>
    </button>
  );
}
export function Notice({
  children,
  tone = "info",
}: {
  children: ReactNode;
  tone?: string;
}) {
  return (
    <div className={`notice ${tone}`}>
      <Icon
        name={tone === "warning" || tone === "error" ? "alert" : "shield"}
        size={18}
      />
      <div>{children}</div>
    </div>
  );
}
export function Empty({
  title = ui.empty,
  children,
  icon = "folder",
  action,
}: {
  title?: string;
  children?: ReactNode;
  icon?: string;
  action?: ReactNode;
}) {
  return (
    <div className="empty">
      <span className="empty-icon">
        <Icon name={icon} size={28} />
      </span>
      <h3>{title}</h3>
      {children && <p>{children}</p>}
      {action}
    </div>
  );
}
export function Loading() {
  return (
    <div className="loading" role="status">
      <span className="spinner" />
      {ui.loading}
    </div>
  );
}
export function ErrorBox({
  message,
  retry,
}: {
  message: string;
  retry?: () => void;
}) {
  return (
    <div role="alert">
      <Notice tone="error">{message}</Notice>
      {retry && <Button onClick={retry}>{ui.retry}</Button>}
    </div>
  );
}
export function Resource({
  r,
  children,
}: {
  r: { loading: boolean; error: string; reload: () => void; data: unknown };
  children: ReactNode;
}) {
  return r.loading ? (
    <Loading />
  ) : r.error ? (
    <ErrorBox message={r.error} retry={r.reload} />
  ) : (
    <>{children}</>
  );
}
export function Thumb({
  thumb,
  name,
  onClick,
}: {
  thumb?: string | null;
  name: string;
  onClick?: () => void;
}) {
  const [failed, setFailed] = useState(false);
  // A thumbnail is addressed by the hash of its own bytes, so it is served as
  // immutable and cached for a year. That is right while the answer is a
  // picture and wrong once — if a browser ever cached a failed or empty
  // response for one of these addresses, it would keep showing that forever
  // and no amount of reloading would help. So a thumbnail that fails is asked
  // for once more, past the cache, before giving up on it.
  const [retry, setRetry] = useState(0);
  useEffect(() => {
    setFailed(false);
    setRetry(0);
  }, [thumb]);
  const content =
    thumb && !failed ? (
      <img
        src={`/api/thumb/${thumb}${retry ? `?again=${retry}` : ""}`}
        alt={name}
        loading="lazy"
        decoding="async"
        onError={() => (retry ? setFailed(true) : setRetry(1))}
      />
    ) : (
      <span className="thumb-placeholder">
        <Icon name="image" />
        <span>{t("net_prevyu")}</span>
      </span>
    );
  return onClick ? (
    <button
      className="thumb"
      onClick={(e) => {
        e.currentTarget.focus();
        onClick();
      }}
      aria-label={t("otkryt", name)}
    >
      {content}
    </button>
  ) : (
    <div className="thumb">{content}</div>
  );
}
/**
 * The evidence behind a score, fetched only when someone opens it.
 *
 * A card can say "metadata from the camera +10" all day; what settles an
 * argument is the model, the lens, the shutter speed and where the date
 * actually came from. Loading it lazily keeps a grid of two hundred frames
 * to two hundred rows, not two hundred requests.
 */
export function FileDetails({ fileId }: { fileId: number }) {
  const r = useResource<FileDetailsRow>(`/file/${fileId}/details`);
  const d = r.data;
  if (r.loading) return <Loading />;
  if (r.error) return <ErrorBox message={r.error} retry={r.reload} />;
  if (!d) return null;
  const m = d.meta;
  const num = (v: number | null | undefined, digits = 2) =>
    v == null ? null : v.toFixed(digits);
  const rows: [string, string | null][] = [
    [t("snyato"), m?.taken_at ? when(m.taken_at) : null],
    [t("otkuda_data"), m?.date_source ? dateSourceName(m.date_source) : null],
    [
      t("kamera"),
      [m?.camera_make, m?.camera_model].filter(Boolean).join(" ") || null,
    ],
    [t("obektiv"), m?.lens || null],
    [t("seriynyy_nomer_kamery"), m?.body_serial || null],
    [
      t("ekspozitsiya"),
      [
        m?.exposure && `${t("vyderzhka_znak")} ${shutter(m.exposure)}`,
        m?.f_number && `f/${m.f_number}`,
        m?.iso && `ISO ${m.iso}`,
        m?.focal_length && `${m.focal_length} ${t("mm")}`,
      ]
        .filter(Boolean)
        .join(" · ") || null,
    ],
    [
      t("koordinaty"),
      m?.gps_lat != null && m?.gps_lon != null
        ? `${m.gps_lat.toFixed(5)}, ${m.gps_lon.toFixed(5)}`
        : null,
    ],
    [t("programma"), m?.software || null],
    [
      t("kadr"),
      d.width && d.height
        ? `${d.width} × ${d.height} · ${d.container?.toUpperCase() || "?"}${
            d.extension_lied ? ` · ${t("rasshirenie_ne_sovpalo")}` : ""
          }`
        : null,
    ],
    [t("na_diske"), `${bytes(d.size)} · ${d.disk}`],
    [
      t("piksely_prochitany"),
      d.pixel_source === "preview"
        ? t("iz_vstroennogo_prevyu")
        : d.pixel_source === "full"
          ? t("polnym_dekodirovaniem")
          : null,
    ],
    [
      t("izmereno"),
      [
        num(d.sharpness, 1) && `${t("rezkost_metrika")} ${num(d.sharpness, 1)}`,
        num(d.contrast, 1) && `${t("kontrast")} ${num(d.contrast, 1)}`,
        num(d.entropy) && `${t("entropiya")} ${num(d.entropy)} ${t("bit")}`,
        num(d.saturation) && `${t("nasyschennost")} ${num(d.saturation)}`,
        num(d.chroma) && `${t("cvetnost")} ${num(d.chroma)}`,
        num(d.tonal_range, 0) &&
          `${t("razmah_tonov")} ${num(d.tonal_range, 0)}`,
      ]
        .filter(Boolean)
        .join(" · ") || null,
    ],
    [
      t("vid_kadra"),
      d.categories.length
        ? d.categories
            .map(
              (c) =>
                `${c.category}${c.manual ? ` (${ui.manual})` : ""} · ${Math.round(c.confidence * 100)}%`,
            )
            .join(", ")
        : null,
    ],
    [t("prichina_propuska"), d.skipped_reason],
  ];
  const known = rows.filter(([, v]) => v);
  return (
    <div className="file-details">
      <code className="path">{d.path}</code>
      {known.length ? (
        <dl>
          {known.map(([label, value]) => (
            <div key={label}>
              <dt>{label}</dt>
              <dd>{value}</dd>
            </div>
          ))}
        </dl>
      ) : (
        <p className="muted">{t("kamera_nichego_ne_zapisala")}</p>
      )}
    </div>
  );
}

/** "1/252.78..." as EXIF stores it, rounded to something a person reads. */
function shutter(raw: string) {
  const m = /^1\/(\d+(?:\.\d+)?)$/.exec(raw);
  if (!m) return raw;
  const d = Number(m[1]);
  return d >= 1 ? `1/${Math.round(d)}` : raw;
}

export function Modal({
  title,
  children,
  onClose,
  wide = false,
}: {
  title: string;
  children: ReactNode;
  onClose: () => void;
  wide?: boolean;
}) {
  const ref = useRef<HTMLDialogElement>(null),
    id = useId();
  useEffect(() => {
    const before = document.activeElement as HTMLElement;
    ref.current?.showModal();
    return () => {
      ref.current?.close();
      before?.focus();
    };
  }, []);
  return (
    <dialog
      className={wide ? "modal wide" : "modal"}
      ref={ref}
      aria-labelledby={id}
      onCancel={(e) => {
        e.preventDefault();
        onClose();
      }}
    >
      <header>
        <h2 id={id}>{title}</h2>
        <Button onClick={onClose} aria-label={ui.close} icon="close" />
      </header>
      <div className="modal-body">{children}</div>
    </dialog>
  );
}
type FolderPickerProps = {
  onChoose: (s: string) => void;
  onClose: () => void;
  initial?: string;
};
/** In the desktop window, the system's own folder dialog; in a browser or on
 *  a NAS, a browser of the folders the server can see. */
export function FolderPicker(p: FolderPickerProps) {
  const [native, setNative] = useState(isDesktop);
  return native ? (
    <NativeFolderPicker {...p} onFailed={() => setNative(false)} />
  ) : (
    <ServerFolderPicker {...p} />
  );
}
function NativeFolderPicker({
  onChoose,
  onClose,
  onFailed,
  initial,
}: FolderPickerProps & { onFailed: () => void }) {
  // One dialog per opening, including under StrictMode's double effect.
  const asked = useRef(false);
  useEffect(() => {
    if (asked.current) return;
    asked.current = true;
    pickFolder(initial, ui.desktop.pickTitle).then(
      (path) => {
        // Cancel chooses nothing and changes nothing.
        if (path) onChoose(path);
        onClose();
      },
      // The dialog could not be shown: the server browser still works.
      () => onFailed(),
    );
  }, []);
  return null;
}
function ServerFolderPicker({
  onChoose,
  onClose,
  initial = "/mnt",
}: FolderPickerProps) {
  const [path, setPath] = useState(initial),
    [input, setInput] = useState(initial);
  const r = useResource<{
    path: string;
    parent: string | null;
    directories: { path: string; name: string }[];
  }>(`/fs?path=${encodeURIComponent(path)}`);
  return (
    <Modal title={t("vybrat_papku_na_servere")} onClose={onClose}>
      <p className="muted">{ui.rootsHint}</p>
      <form
        className="inline"
        onSubmit={(e) => {
          e.preventDefault();
          setPath(input);
        }}
      >
        <input
          aria-label={t("put_na_servere")}
          value={input}
          onChange={(e) => setInput(e.target.value)}
        />
        <Button type="submit">{t("otkryt_2")}</Button>
      </form>
      <Resource r={r}>
        <div className="folder-list">
          {r.data?.parent && (
            <Button
              icon="folder"
              onClick={() => {
                setPath(r.data!.parent!);
                setInput(r.data!.parent!);
              }}
            >
              ..
            </Button>
          )}
          {r.data?.directories.map((d) => (
            <button
              key={d.path}
              onClick={() => {
                setPath(d.path);
                setInput(d.path);
              }}
            >
              <Icon name="folder" />
              {d.name}
              <Icon name="arrow" size={16} />
            </button>
          ))}
        </div>
        <div className="modal-actions">
          <code>{r.data?.path}</code>
          <Button
            kind="primary"
            onClick={() => {
              onChoose(r.data!.path);
              onClose();
            }}
          >
            {t("vybrat_etu_papku")}
          </Button>
        </div>
      </Resource>
    </Modal>
  );
}
// Fixed-height windows keep the DOM bounded even for 50,000 rows.
export function VirtualList<T>({
  items,
  rowHeight = 100,
  height = 560,
  render,
  empty = ui.noResults,
  resetKey,
}: {
  items: T[];
  rowHeight?: number;
  height?: number;
  render: (item: T, index: number) => ReactNode;
  empty?: string;
  /// What makes this a different list. Changing it starts from the top.
  resetKey?: string | number;
}) {
  const [scroll, setScroll] = useState(0);
  const ref = useRef<HTMLDivElement>(null);
  // The given row height is an estimate. A row of photo cards is as tall as
  // the window is wide, so a fixed number either cuts the cards off or leaves
  // a gap under them; the first row that renders says how tall a row is.
  const [measured, setMeasured] = useState(0);
  const rowRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const node = rowRef.current;
    if (!node) return;
    const observer = new ResizeObserver(([entry]) =>
      setMeasured(Math.ceil(entry.contentRect.height)),
    );
    observer.observe(node);
    return () => observer.disconnect();
  });
  // Keyed by what the list *is*, not by the array that carries it. The rows
  // are rebuilt on every render and refetched every few seconds, and going
  // back to the top each time meant a page could not be read to the end.
  useEffect(() => {
    if (ref.current) ref.current.scrollTop = 0;
    setScroll(0);
  }, [resetKey ?? items.length]);
  if (!items.length) return <Empty title={empty} />;
  const row = measured || rowHeight;
  const first = Math.max(0, Math.floor(scroll / row) - 4),
    last = Math.min(items.length, Math.ceil((scroll + height) / row) + 4);
  return (
    <div
      ref={ref}
      className="virtual-list"
      style={{ height: Math.min(height, items.length * row) }}
      onScroll={(e) => setScroll(e.currentTarget.scrollTop)}
    >
      <div style={{ height: items.length * row, position: "relative" }}>
        {items.slice(first, last).map((item, i) => (
          <div
            key={first + i}
            className="virtual-row"
            ref={i === 0 ? rowRef : undefined}
            style={{
              position: "absolute",
              top: (first + i) * row,
              minHeight: row,
              left: 0,
              right: 0,
            }}
          >
            {render(item, first + i)}
          </div>
        ))}
      </div>
    </div>
  );
}
export function Totals({ files, size }: { files: number; size: number }) {
  return (
    <div className="totals">
      <strong>
        {number(files)} <span>{t("faylov")}</span>
      </strong>
      <span className="divider" />
      <strong>{bytes(size)}</strong>
    </div>
  );
}
