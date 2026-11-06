export type AppRoute =
  | "home"
  | "analyze"
  | "scan"
  | "loading"
  | "review"
  | "editor"
  | "export"
  | "exportComplete"
  | "settings";

export const REVIEW_STEPS = [
  "review.stepModInfo",
  "review.stepSourceLanguage",
  "review.stepExistingTarget",
  "review.stepMissingKeys",
  "review.stepAssets",
] as const;

export type ReviewStep = (typeof REVIEW_STEPS)[number];
