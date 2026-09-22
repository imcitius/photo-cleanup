import { useState } from "react";
import { Icon } from "./components";
import { t } from "./i18n";

/** Only the selected pair requests screen-sized frames, never the whole queue. */
export function ReviewPhoto({ id, name }: { id: number; name: string }) {
  const [failed, setFailed] = useState(false);
  return failed ? (
    <span className="review-photo-error">
      <Icon name="image" />
      {t("pe_image_error")}
    </span>
  ) : (
    <img
      className="review-photo"
      src={`/api/file/${id}/preview`}
      alt={name}
      onError={() => setFailed(true)}
    />
  );
}
