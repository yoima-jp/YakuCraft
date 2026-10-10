import { createRoot } from "react-dom/client";
import "../../styles/tokens.css";
import { PreviewDemo } from "./PreviewDemo";

if (import.meta.env.DEV && new URLSearchParams(window.location.search).has("preview-demo")) {
  createRoot(document.getElementById("root")!).render(<PreviewDemo />);
}
