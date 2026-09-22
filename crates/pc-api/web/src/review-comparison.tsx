import { useState } from "react";
import { Button, FileDetails, Icon, Thumb } from "./components";
import { ImageViewer } from "./curation";
import { ReviewPhoto } from "./review-photo";
import { bytes, t, ui, when } from "./i18n";
import type { ReviewGroup } from "./use-review-queue";

export function ReviewComparison({
  group,
  zoom,
  setZoom,
}: {
  group: ReviewGroup;
  zoom: boolean;
  setZoom: (zoom: boolean) => void;
}) {
  const keeper = group.members.find((m) => m.is_keeper) || group.members[0];
  const [file, setFile] = useState(
      group.members.find((m) => m.file_id !== keeper.file_id)?.file_id,
    ),
    [overlay, setOverlay] = useState(false),
    [opacity, setOpacity] = useState(50),
    [full, setFull] = useState(false);
  const candidate = group.members.find((m) => m.file_id === file) || keeper;
  const images =
    keeper.file_id === candidate.file_id ? [keeper] : [keeper, candidate];
  return (
    <section className="queue-comparison" aria-label={ui.compare}>
      <header className="queue-photo-header">
        <div>
          <span className="eyebrow">
            {t("semeystvo")}
            {group.id}
          </span>
          <h2>{keeper.name}</h2>
          <p className="muted">
            {when(group.taken_at)} · {group.camera || ui.unknownCamera}
          </p>
        </div>
        <span className={`badge ${group.exact ? "" : "warning"}`}>
          <Icon name={group.exact ? "check" : "image"} size={14} />
          {t(group.exact ? "rq_exact" : "rq_versions")}
        </span>
      </header>
      <div className="queue-view-tools">
        <div className="segmented">
          <Button
            aria-pressed={!overlay}
            kind={!overlay ? "selected" : ""}
            onClick={() => setOverlay(false)}
          >
            {t("ryadom")}
          </Button>
          <Button
            aria-pressed={overlay}
            kind={overlay ? "selected" : ""}
            onClick={() => setOverlay(true)}
          >
            {t("nalozhenie")}
          </Button>
        </div>
        <div className="inline">
          <Button aria-pressed={!zoom} onClick={() => setZoom(false)}>
            {t("vpisat")}
          </Button>
          <Button
            aria-pressed={zoom}
            kind={zoom ? "selected" : ""}
            onClick={() => setZoom(!zoom)}
          >
            ×2 <kbd>Z</kbd>
          </Button>
          <Button icon="image" onClick={() => setFull(true)}>
            {t("pe_full")}
          </Button>
        </div>
      </div>
      {overlay && (
        <label className="queue-opacity">
          {t("prozrachnost")}
          <input
            type="range"
            min="0"
            max="100"
            value={opacity}
            onChange={(e) => setOpacity(+e.target.value)}
          />
          {opacity}%
        </label>
      )}
      <div
        className={`queue-photos ${overlay ? "overlay" : ""} ${zoom ? "zoom" : ""}`}
        onPointerMove={(e) => {
          if (!zoom) return;
          const rect = e.currentTarget.getBoundingClientRect();
          const half = overlay ? rect.width : rect.width / 2;
          e.currentTarget.style.setProperty(
            "--photo-x",
            `${(((e.clientX - rect.left) % half) / half) * 100}%`,
          );
          e.currentTarget.style.setProperty(
            "--photo-y",
            `${((e.clientY - rect.top) / rect.height) * 100}%`,
          );
        }}
        onPointerLeave={(e) => {
          e.currentTarget.style.removeProperty("--photo-x");
          e.currentTarget.style.removeProperty("--photo-y");
        }}
      >
        {images.map((m, i) => (
          <div
            className="queue-photo"
            key={m.file_id}
            style={overlay && i === 1 ? { opacity: opacity / 100 } : undefined}
          >
            <ReviewPhoto key={m.file_id} id={m.file_id} name={m.name} />
          </div>
        ))}
      </div>
      <div className="queue-captions">
        {images.map((m, i) => (
          <div key={m.file_id}>
            <span className={i === 0 ? "green" : "muted"}>
              {i === 0
                ? ui.willStay
                : group.can_plan
                  ? t("rq_candidate")
                  : t("rq_compare_version")}
            </span>
            <strong>{m.name}</strong>
            <code>{m.dir}</code>
            <small>
              {m.width} × {m.height} · {bytes(m.size)}
            </small>
          </div>
        ))}
      </div>
      <div className="queue-files" aria-label={t("versii_snimka")}>
        {group.members.map((m) => (
          <button
            className={`queue-file ${m.is_keeper ? "keeper" : ""}`}
            key={m.file_id}
            aria-pressed={m.file_id === candidate.file_id}
            disabled={m.is_keeper}
            onClick={() => setFile(m.file_id)}
          >
            <Thumb thumb={m.thumb} name="" />
            <span>
              <strong>{m.is_keeper ? ui.willStay : m.name}</strong>
              <small>
                {m.role_label || m.role} · {bytes(m.size)}
              </small>
            </span>
          </button>
        ))}
      </div>
      <details className="queue-file-details">
        <summary>{t("rq_metadata")}</summary>
        {images.map((m) => (
          <FileDetails key={m.file_id} fileId={m.file_id} />
        ))}
      </details>
      {full && <ImageViewer images={images} onClose={() => setFull(false)} />}
    </section>
  );
}
