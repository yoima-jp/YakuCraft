import { Channel } from "@tauri-apps/api/core";

import { invokeApp } from "../../lib/tauri";
import type { ScanModsFolderEventDto } from "./types";

export function scanModsFolder(path: string, onEvent: (event: ScanModsFolderEventDto) => void) {
  const channel = new Channel<ScanModsFolderEventDto>();
  channel.onmessage = onEvent;
  return invokeApp<string>("scan_mods_folder", { path, channel });
}
