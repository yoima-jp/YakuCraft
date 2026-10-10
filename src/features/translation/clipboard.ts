// 1列のTSVと同じ引用規則を使い、セル内の改行を行の区切りとして貼り付けない。
// 通常の短い訳はそのままコピーし、表計算ソフトの引用されたセルも読み取れるようにする。
export function serializeTranslationClipboard(values: readonly string[]): string {
  return values.map((value) => /["\t\r\n]/.test(value)
    ? `"${value.replaceAll('"', '""')}"`
    : value).join("\n");
}

export function parseTranslationClipboard(value: string): string[] {
  const text = value.replace(/\r\n?/g, "\n");
  const rows: string[] = [];
  const quotedRow = /"((?:[^"]|"")*)"(?:\n|$)/y;
  let offset = 0;
  while (offset <= text.length) {
    if (text[offset] === '"') {
      quotedRow.lastIndex = offset;
      const match = quotedRow.exec(text);
      if (match) {
        rows.push(match[1].replaceAll('""', '"'));
        offset = quotedRow.lastIndex;
        if (!match[0].endsWith("\n")) break;
        continue;
      }
    }
    // 引用形式ではない文章は従来どおり扱い、単独の引用符などを削除しない。
    const newline = text.indexOf("\n", offset);
    if (newline === -1) {
      rows.push(text.slice(offset));
      break;
    }
    rows.push(text.slice(offset, newline));
    offset = newline + 1;
  }
  return rows;
}
