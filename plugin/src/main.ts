import { Notice, Platform, Plugin, setIcon, TAbstractFile } from "obsidian";
import { HttpServerApi, login, type IssuedToken, type VaultInfo } from "./api/http";
import { SyncEngine } from "./engine/engine";
import type { SyncReport } from "./engine/types";
import { confirm, ReportModal } from "./obsidian/modals";
import {
  loadSettings,
  parseExcludedPaths,
  type NotesyncSettings,
} from "./obsidian/settings";
import { NotesyncSettingTab } from "./obsidian/settings-tab";
import { FileStateStore } from "./obsidian/state-store";
import { obsidianTransport } from "./obsidian/transport";
import { ObsidianVaultFs } from "./obsidian/vault-fs";
import { statusText, SyncController, type SyncStatus } from "./sync/controller";
import { TrackingFs } from "./sync/tracking-fs";

export default class NotesyncPlugin extends Plugin {
  // Obsidian 1.13+ declares `settings` on Plugin; this narrows its type.
  declare settings: NotesyncSettings;
  controller!: SyncController;
  private stateStore!: FileStateStore;
  private trackingFs!: TrackingFs;
  private engine: SyncEngine | null = null;
  private engineKey = "";
  private ribbonEl: HTMLElement | null = null;
  private statusBarEl: HTMLElement | null = null;
  private lastReport: SyncReport | null = null;
  private lastReportAt: number | null = null;
  private statusListeners = new Set<(status: SyncStatus) => void>();

  override async onload(): Promise<void> {
    this.settings = loadSettings(await this.loadData());
    if (!this.settings.deviceName) this.settings.deviceName = defaultDeviceName();

    const pluginDir = this.manifest.dir ?? `${this.app.vault.configDir}/plugins/${this.manifest.id}`;
    this.stateStore = new FileStateStore(this.app.vault.adapter, pluginDir);
    this.trackingFs = new TrackingFs(new ObsidianVaultFs(this.app.vault));

    this.controller = new SyncController({
      engine: () => this.getEngine(),
      beforeSync: () => this.refreshTokenIfDue(),
      isOwnChange: (path) => this.trackingFs.isOwnChange(path),
      confirmMassDelete: (err) =>
        confirm(
          this.app,
          "Confirm large delete",
          [
            err.message,
            "This usually means files were deleted on purpose on one device. If you didn't expect it, cancel and check the vault before syncing again. Deleted files stay in the trash, and the server keeps old versions for its history period.",
          ],
          "Delete and sync",
          true,
        ),
      notify: (message) => new Notice(message, 8000),
      onStatus: (status) => this.showStatus(status),
      onReport: (report) => {
        this.lastReport = report;
        this.lastReportAt = Date.now();
      },
    });

    this.ribbonEl = this.addRibbonIcon("refresh-cw", "Sync now (notesync)", () => {
      void this.controller.run("manual");
    });
    // Mobile has no status bar; the ribbon icon shows the state there.
    if (!Platform.isMobile) {
      this.statusBarEl = this.addStatusBarItem();
      this.statusBarEl.addClass("mod-clickable");
      this.statusBarEl.onClickEvent(() => void this.controller.run("manual"));
    }
    this.showStatus(this.controller.currentStatus);

    this.addCommand({
      id: "sync-now",
      name: "Sync now",
      callback: () => void this.controller.run("manual"),
    });
    this.addCommand({
      id: "show-last-sync",
      name: "Show what the last sync did",
      callback: () => new ReportModal(this.app, this.lastReport, this.lastReportAt).open(),
    });

    this.addSettingTab(new NotesyncSettingTab(this.app, this));

    // Registered once the vault has loaded, so the initial file index doesn't
    // look like a burst of edits.
    this.app.workspace.onLayoutReady(() => {
      const changed = (file: TAbstractFile) => this.controller.fileChanged(file.path);
      this.registerEvent(this.app.vault.on("create", changed));
      this.registerEvent(this.app.vault.on("modify", changed));
      this.registerEvent(this.app.vault.on("delete", changed));
      this.registerEvent(
        this.app.vault.on("rename", (file, oldPath) => {
          this.controller.fileChanged(oldPath);
          this.controller.fileChanged(file.path);
        }),
      );
      // Android pauses Obsidian in the background; catch up when it returns.
      this.registerDomEvent(document, "visibilitychange", () => {
        if (document.visibilityState === "visible") this.controller.resumed();
      });
      this.applySchedule();
      if (this.settings.syncOnStartup) void this.controller.run("startup");
    });
  }

  override onunload(): void {
    this.controller?.stop();
  }

  // ---- Settings and account ----

  async saveSettings(): Promise<void> {
    await this.saveData(this.settings);
    this.engine = null; // rebuilt from the new settings on the next sync
    this.applySchedule();
  }

  private applySchedule(): void {
    const s = this.settings;
    this.controller.start({
      editDelayMs: s.syncAfterEdits ? Math.max(1, s.editDelaySeconds) * 1000 : null,
      intervalMs: s.intervalMinutes > 0 ? s.intervalMinutes * 60_000 : null,
    });
  }

  get token(): string | null {
    const id = this.settings.tokenSecretId;
    return id ? this.app.secretStorage.getSecret(id) || null : null;
  }

  get signedIn(): boolean {
    return this.token !== null && this.settings.serverUrl.trim() !== "";
  }

  /** A client for account and vault calls, or null when signed out. */
  api(): HttpServerApi | null {
    const token = this.token;
    if (!token || !this.settings.serverUrl.trim()) return null;
    return new HttpServerApi(this.settings.serverUrl, token, obsidianTransport);
  }

  async signIn(username: string, password: string): Promise<void> {
    const result = await login(
      this.settings.serverUrl,
      username,
      password,
      this.settings.deviceName,
      obsidianTransport,
    );
    this.storeToken(result);
    this.settings.username = result.user.username;
    await this.saveSettings();
  }

  private storeToken(issued: IssuedToken): void {
    if (!this.settings.tokenSecretId) {
      this.settings.tokenSecretId = `notesync-token-${randomId()}`;
    }
    this.app.secretStorage.setSecret(this.settings.tokenSecretId, issued.token);
    this.settings.tokenRefreshAfter = issued.refresh_after;
    this.settings.tokenExpiresAt = issued.expires_at;
  }

  private refreshing: Promise<void> | null = null;

  /**
   * Swaps the device token for a new one when the server says it's due
   * (about once a day), so a copied token soon stops working. Only one swap
   * runs at a time: two at once would leave this device with a replaced token.
   */
  refreshTokenIfDue(): Promise<void> {
    if (Date.now() / 1000 < this.settings.tokenRefreshAfter) return Promise.resolve();
    this.refreshing ??= (async () => {
      try {
        const api = this.api();
        if (!api) return;
        this.storeToken(await api.refresh());
        await this.saveData(this.settings);
        this.engine = null; // rebuilt with the new token
      } finally {
        this.refreshing = null;
      }
    })();
    return this.refreshing;
  }

  async signOut(): Promise<void> {
    const api = this.api();
    try {
      await api?.logout();
    } catch {
      // Signing out locally matters more; the device can also be removed on the server.
    }
    // Secret storage has no delete call, so the token is blanked instead.
    if (this.settings.tokenSecretId) this.app.secretStorage.setSecret(this.settings.tokenSecretId, "");
    this.settings.tokenRefreshAfter = 0;
    this.settings.tokenExpiresAt = 0;
    await this.saveSettings();
    this.showStatus({ kind: "not-configured" });
  }

  /**
   * Connects this vault to a server vault. Switching to a different one
   * forgets the old sync record, so the next sync combines both sides.
   */
  async chooseVault(vault: VaultInfo): Promise<void> {
    if (this.settings.vaultId !== vault.id) await this.stateStore.clear();
    this.settings.vaultId = vault.id;
    this.settings.vaultName = vault.name;
    await this.saveSettings();
  }

  async disconnectVault(): Promise<void> {
    await this.stateStore.clear();
    this.settings.vaultId = null;
    this.settings.vaultName = "";
    await this.saveSettings();
    this.showStatus({ kind: "not-configured" });
  }

  // ---- Sync ----

  private getEngine(): SyncEngine | null {
    const s = this.settings;
    const token = this.token;
    if (!token || !s.serverUrl.trim() || s.vaultId === null) return null;
    const key = JSON.stringify([s.serverUrl, token, s.vaultId, s.deviceName, s.excludedPaths, s.maxFileMb]);
    if (this.engine && this.engineKey === key) return this.engine;
    this.engine = new SyncEngine(
      this.trackingFs,
      new HttpServerApi(s.serverUrl, token, obsidianTransport),
      this.stateStore,
      {
        vaultId: s.vaultId,
        deviceName: s.deviceName,
        excludedPaths: parseExcludedPaths(s.excludedPaths),
        configDir: this.app.vault.configDir,
        maxFileBytes: Math.max(1, s.maxFileMb) * 1024 * 1024,
      },
    );
    this.engineKey = key;
    return this.engine;
  }

  onStatusChange(listener: (status: SyncStatus) => void): () => void {
    this.statusListeners.add(listener);
    return () => this.statusListeners.delete(listener);
  }

  private showStatus(status: SyncStatus): void {
    const text = statusText(status);
    if (this.statusBarEl) this.statusBarEl.setText(`notesync: ${text}`);
    if (this.ribbonEl) {
      const icon =
        status.kind === "error" ? "alert-circle" : status.kind === "not-configured" ? "cloud-off" : "refresh-cw";
      setIcon(this.ribbonEl, icon);
      this.ribbonEl.toggleClass("notesync-syncing", status.kind === "syncing");
      this.ribbonEl.toggleClass("notesync-error", status.kind === "error");
      this.ribbonEl.setAttribute("aria-label", `notesync: ${text}. Select to sync now.`);
    }
    for (const listener of this.statusListeners) listener(status);
  }
}

function defaultDeviceName(): string {
  if (Platform.isAndroidApp) return "Android";
  if (Platform.isIosApp) return "iPhone";
  if (Platform.isWin) return "Windows PC";
  if (Platform.isMacOS) return "Mac";
  if (Platform.isLinux) return "Linux PC";
  return "Device";
}

function randomId(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(8));
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}
