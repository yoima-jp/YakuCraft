import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useRef, useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { Checkbox, Combobox, Dialog, Menu, MenuItem, RadioGroup, Select, Tooltip } from "./Controls";
import { calculateTooltipPosition } from "./Tooltip";
import { useDismiss } from "./useDismiss";

const selectOptions = [
  { value: "alpha", label: "Alpha" },
  { value: "beta", label: "Beta" },
  { value: "gamma", label: "Gamma" },
];

afterEach(cleanup);

describe("Select", () => {
  it("矢印、Home / End、EnterとARIAで項目を選べる", () => {
    const onChange = vi.fn();
    render(<Select aria-label="項目" options={selectOptions} value="alpha" onChange={onChange} />);
    const trigger = screen.getByRole("combobox", { name: "項目" });

    fireEvent.keyDown(trigger, { key: "ArrowDown" });
    expect(trigger.getAttribute("aria-expanded")).toBe("true");
    const listbox = screen.getByRole("listbox", { name: "項目" });
    expect(listbox).toBeTruthy();
    expect(trigger.getAttribute("aria-activedescendant")).toBeTruthy();
    const options = listbox.querySelectorAll('[role="option"]');

    fireEvent.keyDown(trigger, { key: "ArrowDown" });
    expect(trigger.getAttribute("aria-activedescendant")).toBe(options[1].id);
    fireEvent.keyDown(trigger, { key: "ArrowUp" });
    expect(trigger.getAttribute("aria-activedescendant")).toBe(options[0].id);
    fireEvent.keyDown(trigger, { key: "Home" });
    expect(trigger.getAttribute("aria-activedescendant")).toBe(options[0].id);

    fireEvent.keyDown(trigger, { key: "End" });
    expect(trigger.getAttribute("aria-activedescendant")).toBe(options[2].id);
    fireEvent.keyDown(trigger, { key: "Enter" });
    expect(onChange).toHaveBeenCalledWith("gamma");
    expect(trigger.getAttribute("aria-expanded")).toBe("false");
  });

  it("文字入力で移動し、Escで閉じる", () => {
    const onChange = vi.fn();
    render(<Select aria-label="項目" options={selectOptions} value="alpha" onChange={onChange} />);
    const trigger = screen.getByRole("combobox", { name: "項目" });

    fireEvent.keyDown(trigger, { key: "b" });
    expect(trigger.getAttribute("aria-expanded")).toBe("true");
    expect(trigger.getAttribute("aria-activedescendant")).toBe(screen.getByRole("option", { name: "Beta" }).id);
    fireEvent.keyDown(trigger, { key: "Escape" });
    expect(trigger.getAttribute("aria-expanded")).toBe("false");
    expect(onChange).not.toHaveBeenCalled();
  });
});

describe("Combobox", () => {
  it("候補を絞らず一致順に並べ、同じ順位では元の順を保つ", () => {
    const options = ["other", "xABC", "abc-two", "ABC", "zabc", "abc-one", "unrelated"];
    const { rerender } = render(<Combobox id="model" value="abc" options={options} onChange={() => {}} />);
    fireEvent.focus(screen.getByRole("combobox"));
    expect(screen.getAllByRole("option").map((option) => option.textContent)).toEqual([
      "ABC", "abc-two", "abc-one", "xABC", "zabc", "other", "unrelated",
    ]);
    rerender(<Combobox id="model" value="" options={options} onChange={() => {}} />);
    expect(screen.getAllByRole("option").map((option) => option.textContent)).toEqual(options);
  });
});

describe("CheckboxとRadioGroup", () => {
  it("CheckboxはラベルクリックとSpaceキーで状態を切り替える", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    function Harness() {
      const [checked, setChecked] = useState(false);
      return (
        <Checkbox
          checked={checked}
          onChange={(next) => { setChecked(next); onChange(next); }}
        >
          ログを有効にする
        </Checkbox>
      );
    }
    render(<Harness />);
    const input = screen.getByRole("checkbox", { name: "ログを有効にする" }) as HTMLInputElement;

    await user.click(screen.getByText("ログを有効にする"));
    expect(input.checked).toBe(true);
    expect(onChange).toHaveBeenLastCalledWith(true);
    expect(document.activeElement).toBe(input);
    await user.keyboard(" ");
    expect(input.checked).toBe(false);
    expect(onChange).toHaveBeenLastCalledWith(false);
  });

  it("RadioGroupはラベルクリックとSpaceキーで選択を切り替える", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    function Harness() {
      const [value, setValue] = useState("one");
      return (
        <RadioGroup
          legend="動作"
          name="behavior"
          value={value}
          onChange={(next) => { setValue(next); onChange(next); }}
          options={[{ value: "one", label: "1つ" }, { value: "two", label: "2つ" }]}
        />
      );
    }
    render(<Harness />);
    const first = screen.getByRole("radio", { name: "1つ" }) as HTMLInputElement;
    const second = screen.getByRole("radio", { name: "2つ" }) as HTMLInputElement;

    await user.click(screen.getByText("2つ"));
    expect(second.checked).toBe(true);
    expect(onChange).toHaveBeenLastCalledWith("two");
    first.focus();
    expect(document.activeElement).toBe(first);
    await user.keyboard(" ");
    expect(first.checked).toBe(true);
    expect(second.checked).toBe(false);
    expect(onChange).toHaveBeenLastCalledWith("one");
  });
});

describe("DialogとTooltip", () => {
  it("DialogはTabを閉じ込め、Escで閉じる", () => {
    const onClose = vi.fn();
    render(
      <Dialog open title="削除しますか？" onClose={onClose}>
        <button type="button">キャンセル</button>
        <button type="button">削除</button>
      </Dialog>,
    );
    const dialog = screen.getByRole("dialog", { name: "削除しますか？" });
    const cancel = screen.getByRole("button", { name: "キャンセル" });
    const remove = screen.getByRole("button", { name: "削除" });
    expect(document.activeElement).toBe(cancel);

    remove.focus();
    fireEvent.keyDown(remove, { key: "Tab", isComposing: true });
    expect(document.activeElement).toBe(remove);
    fireEvent.keyDown(cancel, { key: "Escape", isComposing: true });
    expect(onClose).not.toHaveBeenCalled();
    fireEvent.keyDown(cancel, { key: "Escape", keyCode: 229, isComposing: false });
    expect(onClose).not.toHaveBeenCalled();

    fireEvent.keyDown(remove, { key: "Tab" });
    expect(document.activeElement).toBe(cancel);
    fireEvent.keyDown(cancel, { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(remove);
    fireEvent.keyDown(dialog, { key: "Escape" });
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("Tooltipはフォーカス時に表示し、説明を操作対象に結び付ける", () => {
    render(
      <Tooltip content="ファイルを選ぶ" shortcut="Ctrl+O">
        <button type="button">開く</button>
      </Tooltip>,
    );
    const button = screen.getByRole("button", { name: "開く" });
    const tooltip = screen.getByRole("tooltip", { hidden: true });

    fireEvent.focus(button);
    expect(tooltip.getAttribute("data-visible")).toBe("true");
    expect(button.getAttribute("aria-describedby")).toContain(tooltip.id);
  });

  it("Tooltipは上に出せないとき下へ反転し、左右端に収める", () => {
    const below = calculateTooltipPosition(
      { top: 4, bottom: 28, left: 2, width: 32 },
      { width: 120, height: 30 },
      { width: 200, height: 400 },
    );
    expect(below).toEqual({ top: 36, left: 8, placement: "below" });

    const above = calculateTooltipPosition(
      { top: 180, bottom: 204, left: 170, width: 24 },
      { width: 120, height: 30 },
      { width: 200, height: 400 },
    );
    expect(above).toEqual({ top: 142, left: 72, placement: "above" });
  });
});

describe("useDismissとMenu", () => {
  it("外側クリック、Escape、フォーカス移動で閉じ、内側クリックでは閉じない", () => {
    const onDismiss = vi.fn();
    function Harness() {
      const rootRef = useRef<HTMLDivElement>(null);
      useDismiss({
        open: true,
        refs: [rootRef],
        onDismiss,
      });
      return (
        <>
          <div data-testid="inside" ref={rootRef}><button type="button">内側</button></div>
          <button
            type="button"
            onPointerDown={(event) => {
              event.preventDefault();
              event.stopPropagation();
            }}
          >
            外側
          </button>
        </>
      );
    }
    render(<Harness />);
    const inside = screen.getByTestId("inside");
    const outside = screen.getByRole("button", { name: "外側" });

    fireEvent.pointerDown(inside);
    expect(onDismiss).not.toHaveBeenCalled();
    fireEvent.pointerDown(outside);
    expect(onDismiss).toHaveBeenLastCalledWith("outside-pointer");
    fireEvent.keyDown(inside, { key: "Escape", isComposing: true });
    expect(onDismiss).toHaveBeenLastCalledWith("outside-pointer");
    fireEvent.keyDown(inside, { key: "Escape", keyCode: 229, isComposing: false });
    expect(onDismiss).toHaveBeenLastCalledWith("outside-pointer");
    fireEvent.keyDown(inside, { key: "Escape" });
    expect(onDismiss).toHaveBeenLastCalledWith("escape");
    fireEvent.focusIn(outside);
    expect(onDismiss).toHaveBeenLastCalledWith("focus-out");
    fireEvent.scroll(document.body);
    expect(onDismiss).toHaveBeenLastCalledWith("scroll");
    fireEvent.resize(window);
    expect(onDismiss).toHaveBeenLastCalledWith("resize");
  });

  it("MenuをEscapeで閉じると開いたボタンへフォーカスを戻す", async () => {
    const user = userEvent.setup();
    render(
      <Menu aria-label="操作" trigger="操作" panelRole="menu">
        <MenuItem>実行</MenuItem>
      </Menu>,
    );
    const trigger = screen.getByRole("button", { name: "操作" });

    trigger.focus();
    await user.keyboard("{ArrowDown}");
    expect(document.activeElement).toBe(screen.getByRole("menuitem", { name: "実行" }));
    await user.keyboard("{Escape}");
    expect(trigger.getAttribute("aria-expanded")).toBe("false");
    expect(document.activeElement).toBe(trigger);
  });

  it("Select内のEscapeでは候補だけ閉じ、次のEscapeで親Menuを閉じる", async () => {
    const user = userEvent.setup();
    render(
      <Menu aria-label="絞り込み" trigger="絞り込み">
        <Select aria-label="状態" options={selectOptions} value="alpha" onChange={() => undefined} />
      </Menu>,
    );
    const menuTrigger = screen.getByRole("button", { name: "絞り込み" });
    await user.click(menuTrigger);
    const selectTrigger = screen.getByRole("combobox", { name: "状態" });
    await user.click(selectTrigger);
    await user.keyboard("{Escape}");
    expect(selectTrigger.getAttribute("aria-expanded")).toBe("false");
    expect(menuTrigger.getAttribute("aria-expanded")).toBe("true");

    await user.keyboard("{Escape}");
    expect(menuTrigger.getAttribute("aria-expanded")).toBe("false");
    expect(document.activeElement).toBe(menuTrigger);
  });
});
