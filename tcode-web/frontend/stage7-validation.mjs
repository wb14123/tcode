import { TimelineStore } from './src/timeline-store.ts';
import { StreamEventBatcher } from './src/stream-event-batcher.ts';
import { ConversationTimelineBuilder } from './src/messages.ts';
import { specialToolArgsPresentation } from './src/tool-args.ts';
import { subagentRowTitle } from './src/timeline-render-helpers.ts';

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

function rawEvent(variant, payload = {}) {
  const raw = { [variant]: payload };
  const rawText = JSON.stringify(raw);
  return {
    rawText,
    rawJson: raw,
    wire: {
      variant,
      payload,
      raw,
      rawText,
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

function validateSubagentInputAggregation() {
  const builder = new ConversationTimelineBuilder();
  builder.appendEvents([
    rawEvent('SubAgentInputStart', {
      msg_id: 7,
      tool_call_id: 'tool-1',
      tool_call_index: 0,
      created_at: 1_700_000_000_000,
      tool_name: 'worker',
    }),
    rawEvent('SubAgentInputChunk', {
      tool_call_index: 0,
      content: 'hello ',
    }),
    rawEvent('SubAgentInputChunk', {
      tool_call_index: 0,
      content: 'world',
    }),
  ]);

  assert(builder.timeline.length === 1, 'pending subagent input renders as one timeline item');
  const pendingItem = builder.timeline[0];
  assert(pendingItem.kind === 'subagent', 'pending subagent input creates a subagent timeline item');
  assert(pendingItem.pending === true, 'subagent input without conversation stays pending');
  assert(pendingItem.input === 'hello world', 'SubAgentInputChunk aggregates into pending subagent input');
  assert(pendingItem.toolCallId === 'tool-1', 'pending subagent preserves tool call id');

  builder.appendEvent(
    rawEvent('SubAgentStart', {
      msg_id: 8,
      conversation_id: 'subagent-1',
      tool_call_id: 'tool-1',
      description: 'worker',
    }),
  );

  assert(builder.timeline.length === 1, 'SubAgentStart reuses the pending subagent timeline item');
  const startedItem = builder.timeline[0];
  assert(startedItem.kind === 'subagent', 'started item remains a subagent timeline item');
  assert(startedItem.pending !== true, 'SubAgentStart clears pending state');
  assert(startedItem.conversationId === 'subagent-1', 'SubAgentStart attaches the real conversation id');
  assert(startedItem.input === 'hello world', 'pending input survives SubAgentStart without duplication');

  builder.appendEvent(
    rawEvent('SubAgentInputChunk', {
      conversation_id: 'subagent-1',
      content: '!',
    }),
  );

  const updatedItem = builder.timeline[0];
  assert(updatedItem.kind === 'subagent', 'updated item remains a subagent timeline item');
  assert(updatedItem.input === 'hello world!', 'SubAgentInputChunk aggregates after conversation id is known');

  builder.appendEvent(
    rawEvent('SubAgentInputChunk', {
      tool_call_index: 0,
      content: '?',
    }),
  );

  assert(builder.timeline.length === 1, 'late index-only SubAgentInputChunk does not create another pending row');
  const lateUpdatedItem = builder.timeline[0];
  assert(lateUpdatedItem.kind === 'subagent', 'late updated item remains a subagent timeline item');
  assert(lateUpdatedItem.input === 'hello world!?', 'late index-only SubAgentInputChunk uses the known subagent row');

  builder.appendEvents([
    rawEvent('SubAgentInputStart', {
      msg_id: 9,
      tool_call_id: 'tool-2',
      tool_call_index: 1,
      created_at: 1_700_000_001_000,
      tool_name: 'worker',
    }),
    rawEvent('SubAgentInputChunk', {
      tool_call_index: 1,
      content: 'world',
    }),
    rawEvent('SubAgentContinue', {
      msg_id: 10,
      conversation_id: 'subagent-1',
      tool_call_id: 'tool-2',
      description: 'worker follow-up',
    }),
  ]);

  assert(builder.timeline.length === 1, 'SubAgentContinue merges pending follow-up into existing subagent row');
  const continuedItem = builder.timeline[0];
  assert(continuedItem.kind === 'subagent', 'continued item remains a subagent timeline item');
  assert(
    continuedItem.input === 'hello world!?\n\nworld',
    'SubAgentContinue preserves follow-up input even when it is a substring of earlier input',
  );
}

function validateSpecialToolArgsPresentation() {
  const search = specialToolArgsPresentation('web_search', JSON.stringify({ query: 'rust async', region: 'us' }));
  assert(search?.collapsedSummary === 'rust async', 'web_search args render a friendly collapsed summary');
  assert(search.expandedText.includes('Query: rust async'), 'web_search expanded args render query field');
  assert(search.expandedText.includes('Extra: {"region":"us"}'), 'web_search expanded args preserve compact extras');

  const fetch = specialToolArgsPresentation(
    'web_fetch',
    JSON.stringify({ url: 'https://example.com', max_length: 5000, skip_chars: 0, empty: '' }),
  );
  assert(fetch?.collapsedSummary === 'https://example.com', 'web_fetch args render a friendly collapsed summary');
  assert(fetch.expandedText.includes('URL: https://example.com'), 'web_fetch expanded args render url field');
  assert(fetch.expandedText.includes('max_length: 5000'), 'web_fetch expanded args render present max_length');
  assert(fetch.expandedText.includes('skip_chars: 0'), 'web_fetch expanded args render present zero skip_chars');
  assert(!fetch.expandedText.includes('empty'), 'web_fetch expanded args omit empty extras');

  assert(specialToolArgsPresentation('web_search', '{') === null, 'invalid JSON falls back to raw args');
  assert(specialToolArgsPresentation('web_search', JSON.stringify({ q: 'rust' })) === null, 'unknown web_search shape falls back to raw args');
  assert(specialToolArgsPresentation('bash', JSON.stringify({ query: 'rust' })) === null, 'unrecognized tools fall back to raw args');
}

function validateSubagentPromptPreview() {
  const taskTitle = subagentRowTitle({
    input: JSON.stringify({ task: 'You are a subagent. Review the mobile UI', model: 'gpt-5.5' }),
    response: '',
    description: 'worker',
  });
  assert(taskTitle === 'Review the mobile UI', 'subagent collapsed title extracts task and strips boilerplate');

  const partialTitle = subagentRowTitle({
    input: '{"model":"gpt-5.5","task":"You are a subagent. Investigate scrolling',
    response: '',
    description: 'worker',
  });
  assert(partialTitle === 'Investigate scrolling', 'subagent collapsed title extracts task from partial streaming JSON');

  const continuedTitle = subagentRowTitle({
    input: JSON.stringify({ conversation_id: 'subagent-1', message: 'Follow up on findings' }),
    response: '',
    description: 'worker',
  });
  assert(continuedTitle === 'Follow up on findings', 'continue_subagent collapsed title extracts message');

  const responseTitle = subagentRowTitle({
    input: JSON.stringify({ conversation_id: 'conversation_abc123456' }),
    response: 'Summarized findings',
    description: 'worker fallback',
  });
  assert(responseTitle === 'Summarized findings', 'subagent collapsed title falls back to response before description');

  const descriptionTitle = subagentRowTitle({
    input: JSON.stringify({ conversation_id: 'conversation_abc123456' }),
    response: '',
    description: 'Analyze backend logs',
  });
  assert(descriptionTitle === 'Analyze backend logs', 'subagent collapsed title falls back to description after prompt and response');

  const internalIdTitle = subagentRowTitle({
    input: '',
    response: '',
    description: 'conversation_abc123456',
  });
  assert(internalIdTitle === 'Waiting for subagent…', 'subagent collapsed title does not show internal conversation ids');
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
validateSubagentInputAggregation();
validateSpecialToolArgsPresentation();
validateSubagentPromptPreview();
await validateStreamEventBatcher();
console.log('stage7 validation passed');
