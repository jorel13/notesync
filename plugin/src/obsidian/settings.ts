export interface NotesyncSettings {
  serverUrl: string;
  /** Shown in the settings; the password is never saved. */
  username: string;
  /** Name for this device on the server and in conflict copy names. */
  deviceName: string;
  /** ID of the device token in Obsidian's secret storage ("" when signed out). */
  tokenSecretId: string;
  /** When the server wants a new token (Unix seconds); 0 means as soon as possible. */
  tokenRefreshAfter: number;
  /** When the current token stops working (Unix seconds); 0 if unknown. */
  tokenExpiresAt: number;
  vaultId: number | null;
  vaultName: string;
  syncOnStartup: boolean;
  syncAfterEdits: boolean;
  editDelaySeconds: number;
  /** 0 turns the timer off. */
  intervalMinutes: number;
  /** One per line: folders end with "/", anything else is an exact file path. */
  excludedPaths: string;
  maxFileMb: number;
}

export const DEFAULT_SETTINGS: NotesyncSettings = {
  serverUrl: "",
  username: "",
  deviceName: "",
  tokenSecretId: "",
  tokenRefreshAfter: 0,
  tokenExpiresAt: 0,
  vaultId: null,
  vaultName: "",
  syncOnStartup: true,
  syncAfterEdits: true,
  editDelaySeconds: 10,
  intervalMinutes: 5,
  excludedPaths: "",
  maxFileMb: 100,
};

/** Parses the excluded-paths text box into the engine's format. */
export function parseExcludedPaths(text: string): string[] {
  return text
    .split(/\r?\n/)
    .map((line) => line.trim().replace(/^\/+/, ""))
    .filter((line) => line.length > 0 && !line.startsWith("#"));
}

/** Reads saved settings, ignoring unknown or wrongly typed values. */
export function loadSettings(saved: unknown): NotesyncSettings {
  const settings: NotesyncSettings = { ...DEFAULT_SETTINGS };
  if (!saved || typeof saved !== "object") return settings;
  const record = saved as Record<string, unknown>;
  for (const key of Object.keys(DEFAULT_SETTINGS) as (keyof NotesyncSettings)[]) {
    const value = record[key];
    const fallback = DEFAULT_SETTINGS[key];
    if (key === "vaultId") {
      if (typeof value === "number" || value === null) settings.vaultId = value;
    } else if (typeof value === typeof fallback) {
      (settings as unknown as Record<string, unknown>)[key] = value;
    }
  }
  return settings;
}
