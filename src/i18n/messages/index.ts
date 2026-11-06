import type { UiLanguage } from "../uiLanguages";
import { common } from "./common";
import { app } from "./app";
import { onboarding } from "./onboarding";
import { home } from "./home";
import { scan } from "./scan";
import { analyze } from "./analyze";
import { review } from "./review";
import { editor } from "./editor";
import { glossary } from "./glossary";
import { entryPreview } from "./entryPreview";
import { preview } from "./preview";
import { exportMessages } from "./export";
import { settings } from "./settings";
import { updater } from "./updater";
import { ai } from "./ai";

export const MESSAGE_CATALOG = {
  common,
  app,
  onboarding,
  home,
  scan,
  analyze,
  review,
  editor,
  glossary,
  entryPreview,
  preview,
  export: exportMessages,
  settings,
  updater,
  ai,
} as const;

export const messages = {
  ja: {
    ...common.ja, ...app.ja, ...onboarding.ja, ...home.ja, ...scan.ja, ...analyze.ja,
    ...review.ja, ...editor.ja, ...glossary.ja, ...entryPreview.ja, ...preview.ja,
    ...exportMessages.ja, ...settings.ja, ...updater.ja, ...ai.ja,
  },
  en: {
    ...common.en, ...app.en, ...onboarding.en, ...home.en, ...scan.en, ...analyze.en,
    ...review.en, ...editor.en, ...glossary.en, ...entryPreview.en, ...preview.en,
    ...exportMessages.en, ...settings.en, ...updater.en, ...ai.en,
  },
  "zh-Hans": {
    ...common["zh-Hans"], ...app["zh-Hans"], ...onboarding["zh-Hans"], ...home["zh-Hans"],
    ...scan["zh-Hans"], ...analyze["zh-Hans"], ...review["zh-Hans"], ...editor["zh-Hans"],
    ...glossary["zh-Hans"], ...entryPreview["zh-Hans"], ...preview["zh-Hans"],
    ...exportMessages["zh-Hans"], ...settings["zh-Hans"], ...updater["zh-Hans"], ...ai["zh-Hans"],
  },
  "zh-Hant": {
    ...common["zh-Hant"], ...app["zh-Hant"], ...onboarding["zh-Hant"], ...home["zh-Hant"],
    ...scan["zh-Hant"], ...analyze["zh-Hant"], ...review["zh-Hant"], ...editor["zh-Hant"],
    ...glossary["zh-Hant"], ...entryPreview["zh-Hant"], ...preview["zh-Hant"],
    ...exportMessages["zh-Hant"], ...settings["zh-Hant"], ...updater["zh-Hant"], ...ai["zh-Hant"],
  },
  ko: {
    ...common.ko, ...app.ko, ...onboarding.ko, ...home.ko, ...scan.ko, ...analyze.ko,
    ...review.ko, ...editor.ko, ...glossary.ko, ...entryPreview.ko, ...preview.ko,
    ...exportMessages.ko, ...settings.ko, ...updater.ko, ...ai.ko,
  },
} satisfies Record<UiLanguage, Record<string, string>>;

export type MessageKey = keyof typeof messages.ja & string;
export type JapaneseMessage<Key extends MessageKey> = typeof messages.ja[Key];
