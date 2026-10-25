import { create } from "zustand";

import type { AppError } from "../../types/commands";
import type { ModScanResultDto, ModpackScanDto, ScanProgressDto } from "./types";

interface ScanStore {
  folderPath: string | null;
  scanRevision: number;
  scanJobId: string | null;
  scanProgress: ScanProgressDto | null;
  results: ModScanResultDto[] | null;
  scannedFolder: string | null;
  instanceDir: string | null;
  looksLikeInstanceDir: boolean;
  modpackScan: ModpackScanDto | null;
  scanError: AppError | null;
  creationComplete: boolean;
  setFolderPath: (path: string) => void;
  retryScan: () => void;
  setScanJobId: (jobId: string | null) => void;
  setScanProgress: (progress: ScanProgressDto | null) => void;
  setScanResults: (results: ModScanResultDto[], scannedFolder: string, instanceDir: string | null, looksLikeInstanceDir: boolean, modpackScan: ModpackScanDto | null) => void;
  setScanError: (error: AppError | null) => void;
  setCreationComplete: (complete: boolean) => void;
}

export const useScanStore = create<ScanStore>()((set) => ({
  folderPath: null,
  scanRevision: 0,
  scanJobId: null,
  scanProgress: null,
  results: null,
  scannedFolder: null,
  instanceDir: null,
  looksLikeInstanceDir: false,
  modpackScan: null,
  scanError: null,
  creationComplete: false,
  setFolderPath: (folderPath) => set((state) => ({
    folderPath,
    scanRevision: state.scanRevision + 1,
    scanJobId: null,
    scanProgress: null,
    results: null,
    scannedFolder: null,
    instanceDir: null,
    looksLikeInstanceDir: false,
    modpackScan: null,
    scanError: null,
    creationComplete: false,
  })),
  retryScan: () => set((state) => ({
    scanRevision: state.scanRevision + 1,
    scanJobId: null,
    scanProgress: null,
    results: null,
    scannedFolder: null,
    instanceDir: null,
    looksLikeInstanceDir: false,
    modpackScan: null,
    scanError: null,
  })),
  setScanJobId: (scanJobId) => set({ scanJobId }),
  setScanProgress: (scanProgress) => set({ scanProgress }),
  setScanResults: (results, scannedFolder, instanceDir, looksLikeInstanceDir, modpackScan) => set({
    results,
    scannedFolder,
    instanceDir,
    looksLikeInstanceDir,
    modpackScan,
    scanProgress: null,
    scanJobId: null,
    scanError: null,
  }),
  setScanError: (scanError) => set({ scanError, scanJobId: null }),
  setCreationComplete: (creationComplete) => set({ creationComplete }),
}));
