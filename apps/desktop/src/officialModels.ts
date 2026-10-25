import bundledCatalog from "./data/official-models.json";

export type OfficialModel = { id: string; displayName: string; isDefault: boolean };
export type OfficialModelCatalog = {
  models: OfficialModel[];
  recommendedModel: string | null;
  source: "local" | "bundled";
  warning: string | null;
};

export function normalizeOfficialModels(value: OfficialModelCatalog): OfficialModelCatalog {
  const seen = new Set<string>();
  const models = value.models.filter(model => {
    if (!model.id.trim() || seen.has(model.id)) return false;
    seen.add(model.id);
    return true;
  });
  return {
    ...value,
    models,
    recommendedModel: models.some(model => model.id === value.recommendedModel)
      ? value.recommendedModel : models.find(model => model.isDefault)?.id ?? null,
  };
}

export const BUNDLED_OFFICIAL_MODELS = normalizeOfficialModels(bundledCatalog as OfficialModelCatalog);
export const INITIAL_OFFICIAL_MODEL = BUNDLED_OFFICIAL_MODELS.recommendedModel ?? "gpt-6.1-sol";

/** Keep an existing/manual model selectable even if the current directory omits it. */
export function officialModelOptions(models: readonly OfficialModel[], currentModel: string): OfficialModel[] {
  return currentModel && !models.some(model => model.id === currentModel)
    ? [{ id: currentModel, displayName: currentModel, isDefault: false }, ...models]
    : [...models];
}
