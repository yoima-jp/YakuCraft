import type { GlossaryTerm } from "../../types/translation";

interface PatternMatch {
  termIndex: number;
  pattern: string[];
}

interface State {
  transitions: Map<string, number>;
  failure: number;
  outputs: PatternMatch[];
}

interface MatchRange {
  start: number;
  end: number;
  termIndex: number;
}

export class GlossaryMatcher {
  private readonly states: State[] = [{ transitions: new Map(), failure: 0, outputs: [] }];
  private readonly terms: readonly GlossaryTerm[];

  constructor(terms: readonly GlossaryTerm[]) {
    this.terms = terms;
    terms.forEach((term, termIndex) => {
      const source = term.sourceTerm.trim();
      if (!source) return;
      const variants = [source, pluralize(source)];
      for (const variant of new Set(variants)) {
        const pattern = Array.from(variant, (character) => character.toLowerCase());
        let stateIndex = 0;
        for (const character of pattern) {
          const next = this.states[stateIndex]?.transitions.get(character);
          if (next !== undefined) {
            stateIndex = next;
          } else {
            const created = this.states.length;
            this.states.push({ transitions: new Map(), failure: 0, outputs: [] });
            this.states[stateIndex]?.transitions.set(character, created);
            stateIndex = created;
          }
        }
        this.states[stateIndex]?.outputs.push({ termIndex, pattern: Array.from(variant) });
      }
    });
    this.buildFailureLinks();
  }

  matchingTerms(source: string): GlossaryTerm[] {
    if (!source || this.states.length === 1) return [];
    const sourceCharacters = Array.from(source);
    let stateIndex = 0;
    const ranges: MatchRange[] = [];
    sourceCharacters.forEach((character, index) => {
      const normalized = character.toLowerCase();
      while (stateIndex !== 0 && !this.states[stateIndex]?.transitions.has(normalized)) {
        stateIndex = this.states[stateIndex]?.failure ?? 0;
      }
      stateIndex = this.states[stateIndex]?.transitions.get(normalized) ?? 0;
      for (const match of this.states[stateIndex]?.outputs ?? []) {
        const start = index + 1 - match.pattern.length;
        const end = index + 1;
        const term = this.terms[match.termIndex];
        if (start < 0 || !term) continue;
        if (isWordCharacter(sourceCharacters[start - 1]) || isWordCharacter(sourceCharacters[end])) continue;
        if (term.caseSensitive && sourceCharacters.slice(start, end).join("") !== match.pattern.join("")) continue;
        ranges.push({ start, end, termIndex: match.termIndex });
      }
    });

    ranges.sort((left, right) => left.start - right.start || right.end - left.end || left.termIndex - right.termIndex);
    const matched: GlossaryTerm[] = [];
    const seen = new Set<number>();
    let selectedEnd = 0;
    for (const range of ranges) {
      if (range.start < selectedEnd) continue;
      if (!seen.has(range.termIndex)) {
        const term = this.terms[range.termIndex];
        if (term) {
          matched.push(term);
          seen.add(range.termIndex);
        }
      }
      selectedEnd = range.end;
    }
    return matched;
  }

  mismatches(source: string | null, target: string): GlossaryTerm[] {
    if (!source || !target) return [];
    return this.matchingTerms(source).filter((term) => !target.includes(term.targetTerm));
  }

  private buildFailureLinks() {
    const queue: number[] = [];
    for (const child of this.states[0]?.transitions.values() ?? []) queue.push(child);
    for (let queueIndex = 0; queueIndex < queue.length; queueIndex += 1) {
      const stateIndex = queue[queueIndex];
      if (stateIndex === undefined) continue;
      const state = this.states[stateIndex];
      if (!state) continue;
      for (const [character, childIndex] of state.transitions) {
        queue.push(childIndex);
        let failure = state.failure;
        while (failure !== 0 && !this.states[failure]?.transitions.has(character)) {
          failure = this.states[failure]?.failure ?? 0;
        }
        const fallback = this.states[failure]?.transitions.get(character);
        const child = this.states[childIndex];
        if (!child) continue;
        child.failure = fallback !== undefined && fallback !== childIndex ? fallback : 0;
        child.outputs.push(...(this.states[child.failure]?.outputs ?? []));
      }
    }
  }
}

function pluralize(source: string) {
  const words = source.split(/(\s+)/);
  let wordIndex = words.length - 1;
  while (wordIndex >= 0 && !words[wordIndex]?.trim()) wordIndex -= 1;
  const word = words[wordIndex] ?? "";
  if (/[^aeiou]y$/i.test(word)) words[wordIndex] = `${word.slice(0, -1)}ies`;
  else if (/(?:s|x|z|ch|sh)$/i.test(word)) words[wordIndex] = `${word}es`;
  else words[wordIndex] = `${word}s`;
  return words.join("");
}

function isWordCharacter(character: string | undefined) {
  return character !== undefined && /[\p{L}\p{N}_]/u.test(character);
}
