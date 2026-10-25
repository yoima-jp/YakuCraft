import type { EntryFlag, EntryKind, TranslationEntry } from "../../types/translation";

export interface PlaceholderValidator {
  hasMismatch(sourceText: string | null, targetText: string, entryKind?: EntryKind): boolean;
}

type PrintfToken = {
  value: string;
  index: number | null;
  signature: string | null;
  reusesPrevious: boolean;
};

const PRINTF_SOURCE = String.raw`%(?:(?<escaped>(?:-?\d+)?%)|(?:(?<index>[1-9]\d*)\$)?(?<flags>[-#+ 0,(<]*)(?<width>\d+)?(?:\.(?<precision>\d+))?(?<conversion>(?:[tT][HIklMSLNpzZsQBbhAaCYyjmedRTrDFc]|[bBhHsScCdoxXeEfgGaA]|n)))`;
const PRINTF_PATTERN = new RegExp(PRINTF_SOURCE, "g");
const PRINTF_AT_START_PATTERN = new RegExp(PRINTF_SOURCE, "y");
const PRINTF_CANDIDATE_PATTERN = /%(?:\d+\$)?[-#+ 0,(<]*\d*(?:\.\d+)?[A-Za-z%]/g;

function isValidPrintf(match: RegExpExecArray): boolean {
  const groups = match.groups ?? {};
  for (const value of [groups.index, groups.width, groups.precision]) {
    if (value !== undefined && Number(value) > 2_147_483_647) return false;
  }

  const flags = groups.flags ?? "";
  if (new Set(flags).size !== flags.length) return false;
  const conversion = groups.conversion ?? "";
  const hasIndex = groups.index !== undefined;
  const hasWidth = groups.width !== undefined;
  const hasPrecision = groups.precision !== undefined;
  if (conversion === "n") return flags === "" && !hasIndex && !hasWidth && !hasPrecision;
  if (hasIndex && flags.includes("<")) return false;
  if (((flags.includes("-") || flags.includes("0")) && !hasWidth)
    || (flags.includes("-") && flags.includes("0"))
    || (flags.includes("+") && flags.includes(" "))) return false;

  const allowedFlags: Record<string, string> = {
    b: "-<", B: "-<", h: "-<", H: "-<", s: "-#<", S: "-#<", c: "-<", C: "-<",
    d: "-+ 0,(<", o: "-#0<", x: "-#0<", X: "-#0<", e: "-#+ 0(<", E: "-#+ 0(<",
    f: "-#+ 0,(<", g: "-+ 0,(<", G: "-+ 0,(<", a: "-#+ 0(<", A: "-#+ 0(<",
    t: "-<", T: "-<",
  };
  const conversionType = conversion.startsWith("t") || conversion.startsWith("T")
    ? conversion[0]!
    : conversion[0]!;
  const allowed = allowedFlags[conversionType];
  if (allowed === undefined || [...flags].some((flag) => !allowed.includes(flag))) return false;
  if ((conversion[0] === "t" || conversion[0] === "T") && hasPrecision) return false;
  if (hasPrecision && "cCdoxXn".includes(conversion[0]!)) return false;
  return true;
}

function isAmbiguousUppercaseWordStart(text: string, index: number): boolean {
  const remainder = text.slice(index);
  const literal = "%Creative";
  return remainder.slice(0, literal.length).toLowerCase() === literal.toLowerCase()
    && !/[A-Za-z0-9]/.test(remainder[literal.length] ?? "");
}

function validPrintfTokens(text: string): PrintfToken[] {
  PRINTF_PATTERN.lastIndex = 0;
  const tokens: PrintfToken[] = [];
  for (const match of text.matchAll(PRINTF_PATTERN)) {
    if (match.index === undefined) continue;
    const groups = match.groups ?? {};
    if (groups.escaped !== undefined) {
      tokens.push({ value: match[0], index: null, signature: null, reusesPrevious: false });
      continue;
    }
    if (!isValidPrintf(match) || isAmbiguousUppercaseWordStart(text, match.index)) continue;

    const conversion = groups.conversion ?? "";
    const lower = conversion[0]!.toLowerCase();
    const signature = conversion[0] === "n"
      ? null
      : conversion[0] === "t" || conversion[0] === "T" ? "datetime" : lower;
    tokens.push({
      value: match[0],
      index: groups.index === undefined ? null : Number(groups.index),
      signature,
      reusesPrevious: (groups.flags ?? "").includes("<"),
    });
  }
  return tokens;
}

function unsupportedPrintfTokens(text: string): string[] {
  PRINTF_CANDIDATE_PATTERN.lastIndex = 0;
  return [...text.matchAll(PRINTF_CANDIDATE_PATTERN)]
    .filter((match) => {
      if (match.index === undefined) return false;
      const naturalPercentOf = /\d+%\s+of\b/g;
      const isNaturalPercentOf = [...text.matchAll(naturalPercentOf)]
        .some((natural) => natural.index !== undefined
          && natural.index + natural[0].indexOf("%") === match.index);
      if (isNaturalPercentOf) return false;
      PRINTF_AT_START_PATTERN.lastIndex = match.index;
      const valid = PRINTF_AT_START_PATTERN.exec(text);
      return valid?.index !== match.index || !isValidPrintf(valid);
    })
    .map(([value]) => value);
}

function printfArgumentDifferences(source: string, target: string): boolean {
  const sourceTokens = validPrintfTokens(source);
  const targetTokens = validPrintfTokens(target);
  const toReferences = (tokens: PrintfToken[]) => {
    const byIndex = new Map<number, Set<string>>();
    const relativeErrors: string[] = [];
    let implicitIndex = 0;
    let previousIndex: number | null = null;
    for (const token of tokens) {
      if (token.signature === null) continue;
      let index: number;
      if (token.reusesPrevious) {
        if (previousIndex === null) {
          relativeErrors.push(token.value);
          continue;
        }
        index = previousIndex;
      } else {
        index = token.index ?? ++implicitIndex;
        previousIndex = index;
      }
      const signatures = byIndex.get(index) ?? new Set<string>();
      signatures.add(token.signature);
      byIndex.set(index, signatures);
    }
    return { byIndex, relativeErrors };
  };

  const sourceReferences = toReferences(sourceTokens);
  const targetReferences = toReferences(targetTokens);
  const sourceRelativeCount = new Map<string, number>();
  for (const value of sourceReferences.relativeErrors) sourceRelativeCount.set(value, (sourceRelativeCount.get(value) ?? 0) + 1);
  for (const value of targetReferences.relativeErrors) {
    const count = sourceRelativeCount.get(value) ?? 0;
    if (count === 0) return true;
    sourceRelativeCount.set(value, count - 1);
  }

  const sourceIndexes = [...sourceReferences.byIndex.keys()];
  const sourceMaxIndex = Math.max(0, ...sourceIndexes);
  for (const [index, signatures] of sourceReferences.byIndex) {
    const targetSignatures = targetReferences.byIndex.get(index);
    if (targetSignatures === undefined || signatures.size !== targetSignatures.size
      || [...signatures].some((signature) => !targetSignatures.has(signature))) return true;
  }
  for (const index of targetReferences.byIndex.keys()) {
    if (index > sourceMaxIndex) return true;
  }

  const sourceInvalid = countValues(unsupportedPrintfTokens(source));
  for (const [value, count] of countValues(unsupportedPrintfTokens(target))) {
    if (count > (sourceInvalid.get(value) ?? 0)) return true;
  }
  return false;
}

function countValues(values: string[]): Map<string, number> {
  const counts = new Map<string, number>();
  for (const value of values) counts.set(value, (counts.get(value) ?? 0) + 1);
  return counts;
}

function balancedEnd(text: string, start: number): number | null {
  const opener = text[start];
  const firstCloser = opener === "{" ? "}" : opener === "[" ? "]" : null;
  if (firstCloser === null) return null;
  const closers = [firstCloser];
  let inString = false;
  let escaped = false;
  for (let index = start + 1; index < text.length; index += 1) {
    const char = text[index]!;
    if (inString) {
      if (escaped) escaped = false;
      else if (char === "\\") escaped = true;
      else if (char === "\"") inString = false;
      continue;
    }
    if (char === "\"") inString = true;
    else if (char === "{") closers.push("}");
    else if (char === "[") closers.push("]");
    else if (char === "}" || char === "]") {
      if (closers.pop() !== char) return null;
      if (closers.length === 0) return index + 1;
    }
  }
  return null;
}

const JSON_COMPONENT_FIELDS = ["\"text\"", "\"translate\"", "\"selector\"", "\"score\"", "\"keybind\"", "\"nbt\""];

function looksLikeJsonTextComponent(candidate: string): boolean {
  const hasComponentField = JSON_COMPONENT_FIELDS.some((field) => candidate.includes(field));
  const objectStart = candidate.indexOf("{");
  const isJsonArray = candidate.startsWith("[")
    && objectStart >= 0
    && candidate.slice(objectStart + 1).trimStart().startsWith("\"");
  return (candidate.startsWith("{") && hasComponentField)
    || isJsonArray;
}

type ProfiledParts = { tokens: string[]; opaqueRanges: Array<[number, number]> };

function profiledParts(text: string, entryKind: EntryKind): ProfiledParts {
  if (entryKind === "patchouli") {
    const matches = [...text.matchAll(/\$\([^\r\n)]*\)|#[^#\s]+#/g)];
    return {
      tokens: matches.map(([token]) => token),
      opaqueRanges: matches.flatMap((match) => match.index === undefined
        ? []
        : [[match.index, match.index + match[0].length] as [number, number]]),
    };
  }
  if (entryKind !== "ftbquests") return { tokens: [], opaqueRanges: [] };

  const tokens: string[] = [];
  const jsonRanges: Array<[number, number]> = [];
  for (let index = 0; index < text.length;) {
    if (text[index] !== "{" && text[index] !== "[") {
      index += 1;
      continue;
    }
    const end = balancedEnd(text, index);
    if (end === null || !looksLikeJsonTextComponent(text.slice(index, end))) {
      index += 1;
      continue;
    }
    jsonRanges.push([index, end]);
    tokens.push(text.slice(index, end));
    index = end;
  }

  const opaqueRanges: Array<[number, number]> = [...jsonRanges];
  for (let index = 0; index < text.length;) {
    if (text[index] !== "{") {
      index += 1;
      continue;
    }
    const end = balancedEnd(text, index);
    if (end === null) {
      index += 1;
      continue;
    }
    const value = text.slice(index, end);
    const inJson = jsonRanges.some(([start, jsonEnd]) => start <= index && end <= jsonEnd);
    if (!inJson && text[index - 1] !== "$" && !/^\{\d+\}$/.test(value)) {
      tokens.push(value);
      opaqueRanges.push([index, end]);
    }
    index = end;
  }

  const isOpaque = (index: number, end: number) => opaqueRanges.some(
    ([start, opaqueEnd]) => start <= index && end <= opaqueEnd,
  );
  for (const match of text.matchAll(/&[0-9a-fk-or]/gi)) {
    if (match.index !== undefined && !isOpaque(match.index, match.index + match[0].length)) {
      tokens.push(match[0]);
    }
  }

  const escapes = /\\[nt\\]|\r\n|\r|\n|\t/g;
  for (const match of text.matchAll(escapes)) {
    if (match.index === undefined || isOpaque(match.index, match.index + match[0].length)) continue;
    const value = match[0];
    tokens.push(value === "\r\n" || value === "\r" || value === "\n" || value === "\\n"
      ? "\\n"
      : value === "\t" || value === "\\t" ? "\\t" : "\\\\");
  }
  return { tokens, opaqueRanges };
}

function maskOpaqueText(text: string, ranges: Array<[number, number]>): string {
  if (ranges.length === 0) return text;
  const sorted = [...ranges].sort(([left], [right]) => left - right);
  let result = "";
  let cursor = 0;
  for (const [start, end] of sorted) {
    if (start < cursor) continue;
    result += text.slice(cursor, start) + " ".repeat(end - start);
    cursor = end;
  }
  return result + text.slice(cursor);
}

function hasTokenMismatch(source: string[], target: string[]): boolean {
  const sourceCounts = countValues(source);
  for (const [token, count] of countValues(target)) {
    if (count > (sourceCounts.get(token) ?? 0)) return true;
  }
  const targetCounts = countValues(target);
  return [...sourceCounts].some(([token, count]) => count > (targetCounts.get(token) ?? 0));
}

// UI 側も Rust と同じ printf の引数比較と、選択された形式タグだけを比較する。
export const mockPlaceholderValidator: PlaceholderValidator = {
  hasMismatch(sourceText, targetText, entryKind = "lang") {
    const source = sourceText ?? "";
    const sourceParts = profiledParts(source, entryKind);
    const targetParts = profiledParts(targetText, entryKind);
    return printfArgumentDifferences(maskOpaqueText(source, sourceParts.opaqueRanges), maskOpaqueText(targetText, targetParts.opaqueRanges))
      || hasTokenMismatch(sourceParts.tokens, targetParts.tokens);
  },
};

export function flagsForTarget(
  entry: TranslationEntry,
  targetText: string,
  validator: PlaceholderValidator = mockPlaceholderValidator,
) {
  const flags: EntryFlag[] = entry.flags.filter((flag) => flag !== "placeholder_error");
  const entryKind = entry.flags.includes("patchouli_text")
    ? "patchouli"
    : entry.entryKind ?? "lang";
  if (validator.hasMismatch(entry.sourceText, targetText, entryKind)) {
    flags.push("placeholder_error");
  }
  return flags;
}
