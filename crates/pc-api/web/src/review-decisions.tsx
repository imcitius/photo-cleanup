import { useResource } from "./api";
import { ErrorBox, Loading } from "./components";
import { number, t } from "./i18n";
interface Summary {
  plan: number;
  keep: number;
  defer: number;
  manual_keepers: number;
  manual_rejects: number;
  folders: { path: string; scope: string }[];
}
export function ReviewDecisions({ revision }: { revision: number }) {
  const r = useResource<Summary>("/review/decisions", revision);
  if (r.error) return <ErrorBox message={r.error} retry={r.reload} />;
  if (!r.data) return <Loading />;
  const d = r.data;
  return (
    <section className="decision-summary" aria-label={t("rq_recorded")}>
      <h3>{t("rq_recorded")}</h3>
      <dl>
        {[
          [t("rq_manual_keepers"), d.manual_keepers],
          [t("rq_manual_rejects"), d.manual_rejects],
          [t("rq_planned"), d.plan],
          [t("rq_kept"), d.keep],
          [t("rq_deferred"), d.defer],
        ].map(([name, count]) => (
          <div key={name}>
            <dt>{name}</dt>
            <dd>{number(Number(count))}</dd>
          </div>
        ))}
      </dl>
      {d.folders.length > 0 && (
        <div className="decision-folder-rules">
          <strong>{t("rq_original_rules")}</strong>
          <ul>
            {d.folders.map((f) => (
              <li key={`${f.scope}/${f.path}`}>
                <code>{f.path}</code>
                <span>
                  {f.scope === "every-root"
                    ? t("rq_all_disks")
                    : t("rq_one_disk")}
                </span>
              </li>
            ))}
          </ul>
        </div>
      )}
      <p className="muted">{t("rq_decisions_help")}</p>
    </section>
  );
}
