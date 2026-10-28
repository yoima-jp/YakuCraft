import { Channel } from "@tauri-apps/api/core";

import { invokeApp } from "../../lib/tauri";
import type { AddUnitsChannelEventDto } from "../../types/commands";
import type { AddInstanceScanEventDto, ScanModsFolderEventDto } from "./types";

export function scanModsFolder(path: string, onEvent: (event: ScanModsFolderEventDto) => void) {
  const channel = new Channel<ScanModsFolderEventDto>();
  channel.onmessage = onEvent;
  return invokeApp<string>("scan_mods_folder", { path, channel });
}

export function scanProjectInstanceSources(projectId: string, onEvent: (event: AddInstanceScanEventDto) => void) {
  const channel = new Channel<AddInstanceScanEventDto>();
  channel.onmessage = onEvent;
  return invokeApp<string>("scan_project_instance_sources", { projectId, channel });
}

export function addModUnits(
  projectId: string,
  paths: string[],
  onEvent: (event: AddUnitsChannelEventDto) => void,
) {
  const channel = new Channel<AddUnitsChannelEventDto>();
  channel.onmessage = onEvent;
  return invokeApp<string>("add_mod_units", { projectId, paths, channel });
}
