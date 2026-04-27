import type { TimelineItem } from './types';

export type TimelineUnsubscribe = () => void;
export type TimelineStructureSubscriber = (structureRevision: number) => void;
export type TimelineItemSubscriber = (item: TimelineItem | undefined, revision: number) => void;
export type TimelineBeforeChangeSubscriber = () => void;

export interface TimelineMutationOptions {
  layoutMayChange?: boolean;
  visibleChange?: boolean;
}

export interface TimelineAddOptions extends TimelineMutationOptions {
  visible?: boolean;
  index?: number;
}

export interface TimelinePatch {
  addedItems?: Array<{ item: TimelineItem; options?: TimelineAddOptions }>;
  updatedItems?: Array<{
    id: string;
    options?: TimelineMutationOptions;
    mutator: (item: TimelineItem) => void;
  }>;
  updatedItemIds?: string[];
  structureChanged?: boolean;
  layoutMayChange?: boolean;
  finalAssistantMessageIds?: string[];
}

interface BatchState {
  depth: number;
  layoutMayChange: boolean;
  beforeChangeEmitted: boolean;
  changedItemIds: Set<string>;
  structureChanged: boolean;
  resetItemIds: Set<string>;
}

export class TimelineStore {
  private visibleIds: string[] = [];
  private itemsById = new Map<string, TimelineItem>();
  private structureRevision = 0;
  private expandedIds = new Set<string>();
  private itemSubscribers = new Map<string, Set<TimelineItemSubscriber>>();
  private structureSubscribers = new Set<TimelineStructureSubscriber>();
  private beforeChangeSubscribers = new Set<TimelineBeforeChangeSubscriber>();
  private sequenceCounter = 0;
  private activeAssistantId: string | null = null;
  private batchState: BatchState | null = null;

  reset(): void {
    const existingItemIds = new Set(this.itemsById.keys());
    this.runInBatch({ layoutMayChange: true }, () => {
      this.visibleIds = [];
      this.itemsById = new Map();
      this.expandedIds = new Set();
      this.sequenceCounter = 0;
      this.activeAssistantId = null;
      this.markStructureChanged();
      for (const id of existingItemIds) {
        this.markItemChanged(id, true);
      }
    });
  }

  getVisibleIds(): readonly string[] {
    return this.visibleIds;
  }

  getVisibleItems(): TimelineItem[] {
    return this.visibleIds.flatMap((id) => {
      const item = this.itemsById.get(id);
      return item ? [item] : [];
    });
  }

  getItem(id: string): TimelineItem | undefined {
    return this.itemsById.get(id);
  }

  getStructureRevision(): number {
    return this.structureRevision;
  }

  getActiveAssistantId(): string | null {
    return this.activeAssistantId;
  }

  setActiveAssistantId(id: string | null): void {
    this.activeAssistantId = id;
  }

  nextSequence(): number {
    const sequence = this.sequenceCounter;
    this.sequenceCounter += 1;
    return sequence;
  }

  nextSequenceId(prefix: string): string {
    return `${prefix}:seq:${this.nextSequence()}`;
  }

  hasItem(id: string): boolean {
    return this.itemsById.has(id);
  }

  isVisible(id: string): boolean {
    return this.visibleIds.includes(id);
  }

  isExpanded(id: string): boolean {
    return this.expandedIds.has(id);
  }

  toggleExpanded(id: string): void {
    if (!this.itemsById.has(id)) {
      return;
    }

    this.runInBatch({ layoutMayChange: true }, () => {
      if (this.expandedIds.has(id)) {
        this.expandedIds.delete(id);
      } else {
        this.expandedIds.add(id);
      }
      this.bumpItemRevision(id);
      this.markItemChanged(id);
    });
  }

  subscribeStructure(callback: TimelineStructureSubscriber): TimelineUnsubscribe {
    this.structureSubscribers.add(callback);
    return () => {
      this.structureSubscribers.delete(callback);
    };
  }

  subscribeItem(id: string, callback: TimelineItemSubscriber): TimelineUnsubscribe {
    let subscribers = this.itemSubscribers.get(id);
    if (!subscribers) {
      subscribers = new Set();
      this.itemSubscribers.set(id, subscribers);
    }
    subscribers.add(callback);
    return () => {
      const current = this.itemSubscribers.get(id);
      current?.delete(callback);
      if (current?.size === 0) {
        this.itemSubscribers.delete(id);
      }
    };
  }

  subscribeBeforeChange(callback: TimelineBeforeChangeSubscriber): TimelineUnsubscribe {
    this.beforeChangeSubscribers.add(callback);
    return () => {
      this.beforeChangeSubscribers.delete(callback);
    };
  }

  addItem(item: TimelineItem, options: TimelineAddOptions = {}): void {
    const visible = options.visible ?? true;
    this.runInBatch({ layoutMayChange: options.layoutMayChange ?? visible }, () => {
      if (this.itemsById.has(item.id)) {
        return;
      }

      item.revision = 0;
      this.itemsById.set(item.id, item);
      if (visible) {
        this.insertVisibleId(item.id, options.index);
      }
    });
  }

  showItem(id: string, options: TimelineMutationOptions & { index?: number } = {}): void {
    if (!this.itemsById.has(id) || this.visibleIds.includes(id)) {
      return;
    }

    this.runInBatch({ layoutMayChange: options.layoutMayChange ?? true }, () => {
      this.insertVisibleId(id, options.index);
    });
  }

  hideItem(id: string, options: TimelineMutationOptions = {}): void {
    const index = this.visibleIds.indexOf(id);
    if (index === -1) {
      return;
    }

    this.runInBatch({ layoutMayChange: options.layoutMayChange ?? true }, () => {
      this.visibleIds.splice(index, 1);
      this.expandedIds.delete(id);
      this.markStructureChanged();
    });
  }

  updateItem(id: string, options: TimelineMutationOptions, mutator: (item: TimelineItem) => void): void {
    const item = this.itemsById.get(id);
    if (!item) {
      return;
    }

    this.runInBatch({ layoutMayChange: options.layoutMayChange ?? false }, () => {
      mutator(item);
      if (options.visibleChange ?? true) {
        this.bumpItemRevision(id);
        this.markItemChanged(id);
      }
    });
  }

  batch(options: TimelineMutationOptions, callback: () => void): void {
    this.runInBatch(options, callback);
  }

  applyPatch(patch: TimelinePatch): void {
    this.runInBatch({ layoutMayChange: patch.layoutMayChange ?? false }, () => {
      for (const addition of patch.addedItems ?? []) {
        this.addItem(addition.item, addition.options);
      }

      for (const update of patch.updatedItems ?? []) {
        this.updateItem(update.id, update.options ?? {}, update.mutator);
      }

      for (const id of patch.updatedItemIds ?? []) {
        if (this.itemsById.has(id)) {
          this.bumpItemRevision(id);
          this.markItemChanged(id);
        }
      }

      if (patch.structureChanged) {
        this.markStructureChanged();
      }
    });
  }

  private runInBatch(options: TimelineMutationOptions, callback: () => void): void {
    const isOuterBatch = this.batchState === null;
    if (isOuterBatch) {
      this.batchState = {
        depth: 0,
        layoutMayChange: false,
        beforeChangeEmitted: false,
        changedItemIds: new Set(),
        structureChanged: false,
        resetItemIds: new Set(),
      };
    }

    const state = this.batchState!;
    state.depth += 1;
    if (options.layoutMayChange) {
      state.layoutMayChange = true;
      this.emitBeforeChangeIfNeeded();
    }

    try {
      callback();
    } finally {
      state.depth -= 1;
      if (isOuterBatch) {
        this.flushBatch(state);
        this.batchState = null;
      }
    }
  }

  private emitBeforeChangeIfNeeded(): void {
    const state = this.batchState;
    if (!state || state.beforeChangeEmitted || !state.layoutMayChange) {
      return;
    }

    state.beforeChangeEmitted = true;
    for (const subscriber of this.beforeChangeSubscribers) {
      subscriber();
    }
  }

  private insertVisibleId(id: string, index: number | undefined): void {
    if (this.visibleIds.includes(id)) {
      return;
    }

    if (index === undefined || index < 0 || index >= this.visibleIds.length) {
      this.visibleIds.push(id);
    } else {
      this.visibleIds.splice(index, 0, id);
    }
    this.markStructureChanged();
  }

  private bumpItemRevision(id: string): void {
    const item = this.itemsById.get(id);
    if (!item) {
      return;
    }
    item.revision += 1;
  }

  private markItemChanged(id: string, reset = false): void {
    const state = this.batchState;
    if (!state) {
      return;
    }
    if (reset) {
      state.resetItemIds.add(id);
    } else {
      state.changedItemIds.add(id);
    }
  }

  private markStructureChanged(): void {
    const state = this.batchState;
    if (!state) {
      return;
    }
    state.structureChanged = true;
  }

  private flushBatch(state: BatchState): void {
    if (state.structureChanged) {
      this.structureRevision += 1;
      for (const subscriber of this.structureSubscribers) {
        subscriber(this.structureRevision);
      }
    }

    const notifiedItemIds = new Set([...state.changedItemIds, ...state.resetItemIds]);
    for (const id of notifiedItemIds) {
      const item = this.itemsById.get(id);
      const revision = item?.revision ?? 0;
      const subscribers = this.itemSubscribers.get(id);
      if (!subscribers) {
        continue;
      }
      for (const subscriber of subscribers) {
        subscriber(item, revision);
      }
    }
  }
}
