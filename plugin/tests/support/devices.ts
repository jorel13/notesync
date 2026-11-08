import { expect } from "vitest";

import { SyncEngine, type SyncOptions } from "../../src/engine/engine";
import type { ServerApi } from "../../src/engine/types";
import { MemoryFs, MemoryStateStore } from "./memory";

/** Fixed time so conflict copy names are predictable. */
export const NOW = new Date(2026, 10, 6, 14, 32);

export interface Device {
  name: string;
  fs: MemoryFs;
  store: MemoryStateStore;
  engine: SyncEngine;
}

/** Pass `from` to reuse another device's files and saved state, e.g. with a different server. */
export function makeDevice(
  api: ServerApi,
  vaultId: number,
  name: string,
  options: Partial<SyncOptions> = {},
  from?: Pick<Device, "fs" | "store">,
): Device {
  const fs = from?.fs ?? new MemoryFs();
  const store = from?.store ?? new MemoryStateStore();
  const engine = new SyncEngine(fs, api, store, {
    vaultId,
    deviceName: name,
    now: () => NOW,
    ...options,
  });
  return { name, fs, store, engine };
}

/** Syncs each device in turn, twice, so every change reaches everyone. */
export async function syncAll(...devices: Device[]): Promise<void> {
  for (let round = 0; round < 2; round++) {
    for (const device of devices) await device.engine.sync();
  }
}

export function expectSameFiles(...devices: Device[]): void {
  const [first, ...rest] = devices;
  for (const other of rest) {
    expect(other.fs.snapshot(), `${other.name} matches ${first.name}`).toEqual(first.fs.snapshot());
  }
}
