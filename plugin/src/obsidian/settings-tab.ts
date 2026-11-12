import { Notice, PluginSettingTab, Setting, type App } from "obsidian";
import type { VaultInfo } from "../api/http";
import { describeError, statusText } from "../sync/controller";
import type NotesyncPlugin from "../main";
import { confirm } from "./modals";

export class NotesyncSettingTab extends PluginSettingTab {
  /** Typed in by the user for one sign-in; never saved. */
  private password = "";
  private newVaultName = "";
  private unsubscribe: (() => void) | null = null;

  constructor(
    app: App,
    private readonly plugin: NotesyncPlugin,
  ) {
    super(app, plugin);
  }

  override display(): void {
    const { containerEl } = this;
    containerEl.empty();
    this.password = "";

    this.serverSection(containerEl);
    this.accountSection(containerEl);
    if (this.plugin.signedIn) this.vaultSection(containerEl);
    this.syncSection(containerEl);
    this.statusSection(containerEl);
  }

  override hide(): void {
    this.unsubscribe?.();
    this.unsubscribe = null;
    this.password = "";
  }

  private get settings() {
    return this.plugin.settings;
  }

  // ---- Server ----

  private serverSection(el: HTMLElement): void {
    new Setting(el).setName("Server").setHeading();
    new Setting(el)
      .setName("Server URL")
      .setDesc(
        this.plugin.signedIn
          ? "Sign out to change the server."
          : "For example https://notes.your-tailnet.ts.net",
      )
      .addText((text) =>
        text
          .setPlaceholder("https://notes.example.ts.net")
          .setValue(this.settings.serverUrl)
          .setDisabled(this.plugin.signedIn)
          .onChange(async (value) => {
            this.settings.serverUrl = value.trim();
            await this.plugin.saveSettings();
          }),
      );
  }

  // ---- Account ----

  private accountSection(el: HTMLElement): void {
    new Setting(el).setName("Account").setHeading();

    if (this.plugin.signedIn) {
      new Setting(el)
        .setName(`Signed in as ${this.settings.username}`)
        .setDesc(
          `This device is called "${this.settings.deviceName}" on the server. ` +
            signInRenewal(this.settings.tokenExpiresAt),
        )
        .addButton((button) =>
          button.setButtonText("Sign out").onClick(async () => {
            const ok = await confirm(
              this.app,
              "Sign out?",
              ["Syncing stops on this device until you sign in again. Your files stay as they are."],
              "Sign out",
            );
            if (!ok) return;
            await this.plugin.signOut();
            this.display();
          }),
        );
      return;
    }

    new Setting(el)
      .setName("Device name")
      .setDesc("Shown on the server and in the names of conflict copies.")
      .addText((text) =>
        text.setValue(this.settings.deviceName).onChange(async (value) => {
          this.settings.deviceName = value.trim() || this.settings.deviceName;
          await this.plugin.saveSettings();
        }),
      );
    new Setting(el).setName("Username").addText((text) =>
      text.setValue(this.settings.username).onChange(async (value) => {
        this.settings.username = value.trim();
        await this.plugin.saveSettings();
      }),
    );
    new Setting(el)
      .setName("Password")
      .setDesc("Used once to sign this device in. It isn't saved.")
      .addText((text) => {
        text.inputEl.type = "password";
        text.inputEl.autocomplete = "current-password";
        text.onChange((value) => (this.password = value));
      });
    new Setting(el).addButton((button) =>
      button
        .setButtonText("Sign in")
        .setCta()
        .onClick(async () => {
          if (!this.settings.serverUrl || !this.settings.username || !this.password) {
            new Notice("Enter the server URL, username and password first.");
            return;
          }
          button.setDisabled(true).setButtonText("Signing in…");
          try {
            await this.plugin.signIn(this.settings.username, this.password);
            new Notice("notesync: signed in. Now choose a vault.");
            this.display();
          } catch (err) {
            new Notice(`notesync: ${signInError(err)}`, 8000);
            button.setDisabled(false).setButtonText("Sign in");
          }
        }),
    );
  }

  // ---- Vault ----

  private vaultSection(el: HTMLElement): void {
    new Setting(el).setName("Server vault").setHeading();
    const current = this.settings.vaultId;

    const choose = new Setting(el)
      .setName("Sync this vault with")
      .setDesc(current === null ? "Not connected yet." : `Connected to "${this.settings.vaultName}".`);
    choose.addDropdown((dropdown) => {
      dropdown.addOption("", "Loading…").setDisabled(true);
      void this.loadVaults().then((vaults) => {
        dropdown.selectEl.empty();
        dropdown.addOption("", vaults ? "Choose a vault" : "Couldn't load vaults");
        for (const vault of vaults ?? []) dropdown.addOption(String(vault.id), vault.name);
        dropdown.setValue(current === null ? "" : String(current));
        dropdown.setDisabled(!vaults);
        dropdown.onChange(async (value) => {
          const vault = vaults?.find((v) => String(v.id) === value);
          if (!vault || vault.id === current) return;
          if (!(await this.connect(vault))) dropdown.setValue(current === null ? "" : String(current));
        });
      });
    });

    new Setting(el)
      .setName("Create a new server vault")
      .setDesc("Creates an empty vault on the server and connects this vault to it.")
      .addText((text) =>
        text.setPlaceholder("Vault name").onChange((value) => (this.newVaultName = value.trim())),
      )
      .addButton((button) =>
        button.setButtonText("Create").onClick(async () => {
          const api = this.plugin.api();
          if (!api || !this.newVaultName) {
            new Notice("Enter a name for the new vault.");
            return;
          }
          try {
            const vault = await api.createVault(this.newVaultName);
            await this.connect(vault);
          } catch (err) {
            new Notice(`notesync: ${describeError(err)}`, 8000);
          }
        }),
      );

    if (current !== null) {
      new Setting(el)
        .setName("Disconnect")
        .setDesc("Stops syncing this vault. Files on this device and on the server are kept.")
        .addButton((button) =>
          button.setButtonText("Disconnect").onClick(async () => {
            await this.plugin.disconnectVault();
            this.display();
          }),
        );
    }
  }

  private async loadVaults(): Promise<VaultInfo[] | null> {
    try {
      return (await this.plugin.api()?.listVaults()) ?? null;
    } catch (err) {
      new Notice(`notesync: ${describeError(err)}`, 8000);
      return null;
    }
  }

  /** Asks for confirmation, connects, then runs the first sync. */
  private async connect(vault: VaultInfo): Promise<boolean> {
    const switching = this.settings.vaultId !== null;
    const ok = await confirm(
      this.app,
      `Sync with "${vault.name}"?`,
      [
        "The first sync combines this vault with the server vault: files that exist only here are uploaded, files that exist only on the server are downloaded, and identical files are linked.",
        "If the same file differs on both sides, the server's version stays in place and this device's version is kept as a conflict copy. Nothing is deleted.",
        ...(switching ? ["This replaces the current connection."] : []),
        "Consider making a backup copy of this vault before the first sync.",
      ],
      "Connect and sync",
    );
    if (!ok) return false;
    await this.plugin.chooseVault(vault);
    this.display();
    void this.plugin.controller.run("manual");
    return true;
  }

  // ---- Sync options ----

  private syncSection(el: HTMLElement): void {
    new Setting(el).setName("When to sync").setHeading();
    new Setting(el).setName("When Obsidian starts").addToggle((toggle) =>
      toggle.setValue(this.settings.syncOnStartup).onChange(async (value) => {
        this.settings.syncOnStartup = value;
        await this.plugin.saveSettings();
      }),
    );
    new Setting(el)
      .setName("After edits")
      .setDesc("Syncs once you've stopped editing for the delay below.")
      .addToggle((toggle) =>
        toggle.setValue(this.settings.syncAfterEdits).onChange(async (value) => {
          this.settings.syncAfterEdits = value;
          await this.plugin.saveSettings();
        }),
      );
    this.numberSetting(el, "Delay after edits (seconds)", "", "editDelaySeconds", 1);
    this.numberSetting(el, "Every N minutes", "0 turns the timer off.", "intervalMinutes", 0);

    new Setting(el).setName("What to sync").setHeading();
    new Setting(el)
      .setName("Excluded paths")
      .setDesc(
        "One per line. End folders with / (for example Private/). .obsidian/, .trash/ and .git/ are never synced.",
      )
      .addTextArea((area) => {
        area.setValue(this.settings.excludedPaths).onChange(async (value) => {
          this.settings.excludedPaths = value;
          await this.plugin.saveSettings();
        });
        area.inputEl.rows = 4;
      });
    this.numberSetting(
      el,
      "Largest file (MB)",
      "Larger files aren't uploaded or downloaded on this device. Large downloads are fetched a piece at a time, but uploading a file reads all of it into memory, so a phone may need a lower limit. The server has its own limit too.",
      "maxFileMb",
      1,
    );
  }

  private numberSetting(
    el: HTMLElement,
    name: string,
    desc: string,
    key: "editDelaySeconds" | "intervalMinutes" | "maxFileMb",
    min: number,
  ): void {
    new Setting(el)
      .setName(name)
      .setDesc(desc)
      .addText((text) => {
        text.inputEl.type = "number";
        text.inputEl.min = String(min);
        text.setValue(String(this.settings[key])).onChange(async (value) => {
          const n = Number(value);
          if (!Number.isFinite(n) || n < min) return;
          this.settings[key] = Math.round(n);
          await this.plugin.saveSettings();
        });
      });
  }

  // ---- Status ----

  private statusSection(el: HTMLElement): void {
    new Setting(el).setName("Status").setHeading();
    const status = new Setting(el)
      .setName(statusText(this.plugin.controller.currentStatus))
      // Shows which build Obsidian actually loaded, to confirm an update took.
      .setDesc(`Plugin version ${this.plugin.manifest.version}`)
      .addButton((button) =>
        button.setButtonText("Sync now").onClick(() => void this.plugin.controller.run("manual")),
      );
    this.unsubscribe?.();
    this.unsubscribe = this.plugin.onStatusChange((s) => status.setName(statusText(s)));
  }
}

function signInRenewal(expiresAt: number): string {
  if (!expiresAt) return "The sign-in renews itself on the next sync.";
  const date = new Date(expiresAt * 1000).toLocaleDateString();
  return `The sign-in renews itself as it syncs. If this device doesn't sync before ${date}, you'll need to sign in again.`;
}

function signInError(err: unknown): string {
  const message = describeError(err);
  // The server answers a wrong password with "unauthorized" too.
  if (message.startsWith("Signed out by the server")) return "Wrong username or password.";
  return message;
}
