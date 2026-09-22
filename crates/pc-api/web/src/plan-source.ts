export type PlanSource = "reviewed" | "originals" | "automatic";

export function savedPlanSource(): PlanSource {
  const saved = sessionStorage.getItem("pc-plan-source");
  if (saved === "reviewed" || saved === "originals" || saved === "automatic")
    return saved;
  return sessionStorage.getItem("pc-reviewed-plan") === "true"
    ? "reviewed"
    : "automatic";
}
export function savePlanSource(source: PlanSource) {
  sessionStorage.setItem("pc-plan-source", source);
}
export function openPlan(source: PlanSource) {
  savePlanSource(source);
  location.hash = "plan";
}
