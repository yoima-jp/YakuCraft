import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { TargetLanguagePicker } from "./TargetLanguagePicker";

afterEach(cleanup);

function Harness({ initial = "ja_jp", isOptionDisabled }: { initial?: string; isOptionDisabled?: (code: string) => boolean }) {
  const [value, setValue] = useState(initial);
  const onChange = vi.fn((next: string) => setValue(next));
  return (
    <>
      <TargetLanguagePicker
        aria-label="翻訳先の言語"
        value={value}
        onChange={onChange}
        isOptionDisabled={isOptionDisabled}
        disabledOptionReason="PLaMo 非対応"
      />
      <output data-testid="value">{value}</output>
    </>
  );
}

describe("TargetLanguagePicker", () => {
  it("無効な候補は選択できず、理由付きで aria-disabled になる", () => {
    render(<Harness isOptionDisabled={(code) => code === "af_za"} />);

    fireEvent.focus(screen.getByRole("combobox", { name: "翻訳先の言語" }));
    const disabled = screen.getByRole("option", { name: /Afrikaans/ });
    expect(disabled.getAttribute("aria-disabled")).toBe("true");
    expect(disabled.textContent).toContain("PLaMo 非対応");

    fireEvent.click(disabled);
    expect(screen.getByTestId("value").textContent).toBe("ja_jp");

    const enabled = screen.getByRole("option", { name: /Deutschland/ });
    expect(enabled.getAttribute("aria-disabled")).toBeNull();
    fireEvent.click(enabled);
    expect(screen.getByTestId("value").textContent).toBe("de_de");
  }, 15_000);

  it("矢印キーは無効な候補を飛ばし、Enterでも無効な候補を選ばない", () => {
    // 先頭の af_za だけを無効にし、開いた直後の位置が次の候補へ移ることを確かめる。
    render(<Harness isOptionDisabled={(code) => code === "af_za" || code === "ar_sa"} />);
    const input = screen.getByRole("combobox", { name: "翻訳先の言語" });

    fireEvent.focus(input);
    const listbox = screen.getByRole("listbox", { name: "翻訳先の言語" });
    const options = [...listbox.querySelectorAll<HTMLElement>('[role="option"]')];
    const activeId = () => input.getAttribute("aria-activedescendant");
    expect(activeId()).not.toBe(options[0].id);
    expect(activeId()).not.toBe(options[1].id);

    fireEvent.keyDown(input, { key: "ArrowUp" });
    expect(activeId()).not.toBe(options[0].id);
    expect(activeId()).not.toBe(options[1].id);

    fireEvent.keyDown(input, { key: "Enter" });
    expect(screen.getByTestId("value").textContent).not.toBe("af_za");
    expect(screen.getByTestId("value").textContent).not.toBe("ar_sa");
  }, 15_000);

  it("無効な候補しか無い場合も、選択済みの値を表示し続ける", () => {
    render(<Harness isOptionDisabled={() => true} />);

    expect((screen.getByRole("combobox", { name: "翻訳先の言語" }) as HTMLInputElement).value).toContain("日本語");
  }, 15_000);
});
