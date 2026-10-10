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
  "Mod情報",
  "翻訳元",
  "日本語訳",
  "不足キー",
  "アセット",
] as const;

export type ReviewStep = (typeof REVIEW_STEPS)[number];
