import { Modal, Setting, type App } from "obsidian";
import type { SyncReport } from "../engine/types";

/** Asks a yes/no question. Resolves true only if the confirm button is chosen. */
export function confirm(
  app: App,
  title: string,
  body: string[],
  confirmText: string,
  danger = false,
): Promise<boolean> {
  return new Promise((resolve) => {
    new ConfirmModal(app, title, body, confirmText, danger, resolve).open();
  });
}

class ConfirmModal extends Modal {
  private answered = false;

  constructor(
    app: App,
    private readonly title: string,
    private readonly body: string[],
    private readonly confirmText: string,
    private readonly danger: boolean,
    private readonly resolve: (value: boolean) => void,
  ) {
    super(app);
  }

  override onOpen(): void {
    this.setTitle(this.title);
    for (const paragraph of this.body) this.contentEl.createEl("p", { text: paragraph });
    new Setting(this.contentEl)
      .addButton((button) => button.setButtonText("Cancel").onClick(() => this.answer(false)))
      .addButton((button) => {
        button.setButtonText(this.confirmText).onClick(() => this.answer(true));
        if (this.danger) button.setWarning();
        else button.setCta();
      });
  }

  private answer(value: boolean): void {
    this.answered = true;
    this.resolve(value);
    this.close();
  }

  override onClose(): void {
    this.contentEl.empty();
    if (!this.answered) this.resolve(false);
  }
}

/** Lists what the last sync did. */
export class ReportModal extends Modal {
  constructor(
    app: App,
    private readonly report: SyncReport | null,
    private readonly finishedAt: number | null,
  ) {
    super(app);
  }

  override onOpen(): void {
    this.setTitle("Last sync");
    const { contentEl } = this;
    if (!this.report) {
      contentEl.createEl("p", { text: "No sync has finished since Obsidian started." });
      return;
    }
    if (this.finishedAt !== null) {
      contentEl.createEl("p", { text: `Finished ${new Date(this.finishedAt).toLocaleString()}.` });
    }
    const r = this.report;
    const sections: [string, string[]][] = [
      ["Conflict copies", r.conflictCopies],
      ["Merged", r.merged],
      ["Downloaded", r.downloaded],
      ["Uploaded", r.uploaded],
      ["Renamed on this device", r.renamedLocally.map((x) => `${x.from} → ${x.to}`)],
      ["Moved to trash on this device", r.deletedLocally],
      ["Deleted on the server", r.deletedRemotely],
      ["Skipped", r.skipped.map((x) => `${x.path}: ${x.reason}`)],
    ];
    let any = false;
    for (const [heading, items] of sections) {
      if (items.length === 0) continue;
      any = true;
      contentEl.createEl("h4", { text: `${heading} (${items.length})` });
      const list = contentEl.createEl("ul");
      for (const item of items) list.createEl("li", { text: item });
    }
    if (!any) contentEl.createEl("p", { text: "Everything was already up to date." });
  }

  override onClose(): void {
    this.contentEl.empty();
  }
}
