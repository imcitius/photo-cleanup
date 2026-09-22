export type PlanSource = "reviewed" | "originals" | "all";

export function savedPlanSource(): PlanSource {
  const saved = sessionStorage.getItem("pc-plan-source");
  if (saved === "reviewed" || saved === "originals" || saved === "all")
    return saved;
  return "all";
}
export function savePlanSource(source: PlanSource) {
  sessionStorage.setItem("pc-plan-source", source);
}
export function openPlan(source: PlanSource) {
  savePlanSource(source);
  location.hash = "plan";
}
