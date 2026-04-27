import { TimelineStore } from './src/timeline-store.ts';
import { StreamEventBatcher } from './src/stream-event-batcher.ts';

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function userItem(id = 'user:1') {
  return {
    id,
    revision: 0,
    kind: 'user',
    msgId: 1,
    createdAt: null,
    content: 'hello',
  };
}

function rawEvent(variant) {
  return {
    rawText: variant,
    rawJson: { [variant]: {} },
    wire: {
      variant,
      payload: {},
      raw: { [variant]: {} },
      rawText: variant,
    },
  };
}

function validateTimelineStore() {
  const store = new TimelineStore();
  const events = [];
  let itemNotifications = 0;
  let structureNotifications = 0;

  store.subscribeBeforeChange(() => events.push('before'));
  store.subscribeStructure(() => {
    structureNotifications += 1;
    events.push('structure');
  });
  store.subscribeItem('user:1', (item) => {
    itemNotifications += 1;
    events.push(`item:${item?.revision ?? 'missing'}`);
  });

  store.addItem(userItem(), { visible: true, layoutMayChange: true });
  assert(store.getStructureRevision() === 1, 'adding visible item increments structure revision');
  assert(structureNotifications === 1, 'adding visible item notifies structure subscriber');
  assert(events[0] === 'before', 'before-change fires before visible add notifications');
  assert(store.getItem('user:1')?.revision === 0, 'new item starts at revision 0');

  events.length = 0;
  store.updateItem('user:1', { layoutMayChange: true }, (item) => {
    item.content = 'updated';
  });
  assert(store.getStructureRevision() === 1, 'item update does not increment structure revision');
  assert(structureNotifications === 1, 'item update does not notify structure subscriber');
  assert(itemNotifications === 1, 'item update notifies item subscriber');
  assert(store.getItem('user:1')?.revision === 1, 'item update increments item revision');
  assert(events.join(',') === 'before,item:1', 'item update notification order is before then item');

  store.toggleExpanded('user:1');
  assert(store.isExpanded('user:1'), 'toggleExpanded stores expansion state');
  assert(store.getItem('user:1')?.revision === 2, 'toggleExpanded increments item revision');
  assert(structureNotifications === 1, 'toggleExpanded does not notify structure subscriber');

  store.reset();
  assert(store.getVisibleIds().length === 0, 'reset clears visible ids');
  assert(store.getItem('user:1') === undefined, 'reset clears item records');
  assert(!store.isExpanded('user:1'), 'reset clears expansion state');
}

async function validateStreamEventBatcher() {
  let nextFrame = 1;
  const callbacks = new Map();
  globalThis.window = {
    requestAnimationFrame(callback) {
      const id = nextFrame;
      nextFrame += 1;
      callbacks.set(id, callback);
      return id;
    },
    cancelAnimationFrame(id) {
      callbacks.delete(id);
    },
  };

  const batches = [];
  const batcher = new StreamEventBatcher((events) => batches.push(events.map((event) => event.wire?.variant ?? 'raw')));
  batcher.enqueue(rawEvent('AssistantMessageChunk'));
  batcher.enqueue(rawEvent('ToolOutputChunk'));
  assert(batches.length === 0, 'ordinary events wait for animation frame');
  const callback = callbacks.values().next().value;
  callback();
  assert(batches.length === 1, 'ordinary events flush once on animation frame');
  assert(batches[0].join(',') === 'AssistantMessageChunk,ToolOutputChunk', 'ordinary events batch together');

  batcher.enqueue(rawEvent('AssistantMessageChunk'));
  batcher.enqueue(rawEvent('AssistantMessageEnd'));
  assert(batches.length === 3, 'final assistant event flushes pending first and then applies immediately');
  assert(batches[1].join(',') === 'AssistantMessageChunk', 'pending chunk flushes before final');
  assert(batches[2].join(',') === 'AssistantMessageEnd', 'final event applies as its own batch');
}

validateTimelineStore();
await validateStreamEventBatcher();
console.log('stage7 validation passed');
