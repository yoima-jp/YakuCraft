type StringMessages = Record<string, string>;
type ExactMessages<Actual extends StringMessages, Japanese extends StringMessages> =
  Actual &
  { [Key in Exclude<keyof Actual, keyof Japanese>]: never } &
  { [Key in Exclude<keyof Japanese, keyof Actual>]: string };

export function defineMessages<
  const Japanese extends StringMessages,
  const English extends StringMessages,
  const SimplifiedChinese extends StringMessages,
  const TraditionalChinese extends StringMessages,
  const Korean extends StringMessages,
>(messages: {
  ja: Japanese;
  en: ExactMessages<English, Japanese>;
  "zh-Hans": ExactMessages<SimplifiedChinese, Japanese>;
  "zh-Hant": ExactMessages<TraditionalChinese, Japanese>;
  ko: ExactMessages<Korean, Japanese>;
}): {
  ja: Japanese;
  en: English;
  "zh-Hans": SimplifiedChinese;
  "zh-Hant": TraditionalChinese;
  ko: Korean;
} {
  return messages;
}

export type MessageArguments<Text extends string> =
  [PlaceholderNames<Text>] extends [never]
    ? []
    : [params: { [Name in PlaceholderNames<Text>]: string | number }];

type PlaceholderNames<Text extends string> =
  Text extends `${string}{${infer Name}}${infer Tail}`
    ? Name | PlaceholderNames<Tail>
    : never;
