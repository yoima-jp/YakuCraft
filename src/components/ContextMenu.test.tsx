import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  ContextMenu,
  ContextMenuItem,
  useContextMenuTrigger,
} from "./ContextMenu";

afterEach(cleanup);

function KeyboardMenu({ onSelect }: { onSelect: () => void }) {
  const menu = useContextMenuTrigger();
  return (
    <>
      <button type="button" {...menu.triggerProps}>行</button>
      {menu.position && (
        <ContextMenu label="行の操作" position={menu.position} onClose={menu.close}>
          <ContextMenuItem onClick={onSelect}>先頭</ContextMenuItem>
          <ContextMenuItem>次の項目</ContextMenuItem>
        </ContextMenu>
      )}
    </>
  );
}

describe("ContextMenu", () => {
  it("Shift+F10で開き、矢印キーで移動し、Escapeで起点へ戻る", () => {
    const { getByRole } = render(<KeyboardMenu onSelect={vi.fn()} />);
    const trigger = screen.getByRole("button", { name: "行" });
    trigger.focus();
    fireEvent.keyDown(trigger, { key: "F10", shiftKey: true });

    const first = getByRole("menuitem", { name: "先頭" });
    const second = getByRole("menuitem", { name: "次の項目" });
    expect(document.activeElement).toBe(first);
    fireEvent.keyDown(first, { key: "ArrowDown" });
    expect(document.activeElement).toBe(second);
    fireEvent.keyDown(second, { key: "Escape" });
    expect(screen.queryByRole("menu")).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });

  it("Enterで選択して閉じる", async () => {
    const onSelect = vi.fn();
    const user = userEvent.setup();
    render(<KeyboardMenu onSelect={onSelect} />);
    const trigger = screen.getByRole("button", { name: "行" });
    trigger.focus();
    fireEvent.keyDown(trigger, { key: "ContextMenu" });
    await user.keyboard("{Enter}");
    expect(onSelect).toHaveBeenCalledOnce();
    expect(screen.queryByRole("menu")).toBeNull();
  });
});
