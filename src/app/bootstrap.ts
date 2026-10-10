import { StrictMode, createElement } from "react";
import { createRoot } from "react-dom/client";

import { App } from "./App";

const rootElement = document.getElementById("root");

if (rootElement === null) {
  throw new Error("アプリの表示領域を見つけられませんでした。");
}

createRoot(rootElement).render(
  createElement(StrictMode, null, createElement(App)),
);
