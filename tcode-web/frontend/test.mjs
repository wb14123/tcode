import { marked } from 'marked';
import katex from 'katex';
// markdown.ts registers its math tokenizers with marked as a side effect of import.
import './src/markdown.ts';
import { TimelineStore } from './src/timeline-store.ts';
import { StreamEventBatcher } from './src/stream-event-batcher.ts';
import { ConversationTimelineBuilder, parseStreamLine } from './src/messages.ts';
import { specialToolArgsPresentation } from './src/tool-args.ts';
import { subagentRowTitle } from './src/timeline-render-helpers.ts';
import { mediaUrl } from './src/api.ts';
import { shouldSubmitOnEnter } from './src/composer-input.ts';

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

function validateActiveWorkState() {
  const toolBuilder = new ConversationTimelineBuilder();
  toolBuilder.appendEvent(rawEvent('ToolRequestPermission', { tool_call_id: 'tool-busy' }));
  assert(toolBuilder.hasActiveWork(), 'tool permission wait marks the conversation active');
  toolBuilder.appendEvent(rawEvent('ToolMessageEnd', { tool_call_id: 'tool-busy', end_status: 'success' }));
  assert(!toolBuilder.hasActiveWork(), 'tool end clears permission wait activity');

  const subagentBuilder = new ConversationTimelineBuilder();
  subagentBuilder.appendEvent(
    rawEvent('SubAgentStart', {
      msg_id: 1,
      conversation_id: 'subagent-active',
      tool_call_id: 'tool-1',
      description: 'worker',
    }),
  );
  assert(subagentBuilder.hasActiveWork(), 'SubAgentStart marks the subagent active');
  subagentBuilder.appendEvent(
    rawEvent('SubAgentTurnEnd', {
      msg_id: 2,
      conversation_id: 'subagent-active',
      end_status: 'success',
      response: 'done',
    }),
  );
  assert(!subagentBuilder.hasActiveWork(), 'SubAgentTurnEnd marks the subagent idle');
  let item = subagentBuilder.timeline[0];
  assert(item.kind === 'subagent' && item.endStatus === 'success', 'SubAgentTurnEnd stores the final status');

  subagentBuilder.appendEvent(
    rawEvent('SubAgentContinue', {
      msg_id: 3,
      conversation_id: 'subagent-active',
      tool_call_id: 'tool-2',
      description: 'worker follow-up',
    }),
  );
  assert(subagentBuilder.hasActiveWork(), 'SubAgentContinue marks an idle subagent active again');
  item = subagentBuilder.timeline[0];
  assert(item.kind === 'subagent' && item.endStatus === null, 'SubAgentContinue clears the old final status');

  subagentBuilder.appendEvent(rawEvent('SubAgentWaitingPermission', { conversation_id: 'subagent-active' }));
  assert(subagentBuilder.hasActiveWork(), 'subagent permission wait remains active');
  subagentBuilder.appendEvent(rawEvent('SubAgentPermissionDenied', { conversation_id: 'subagent-active' }));
  assert(subagentBuilder.hasActiveWork(), 'subagent permission denial returns to active running state');
  subagentBuilder.appendEvent(
    rawEvent('SubAgentTurnEnd', {
      msg_id: 4,
      conversation_id: 'subagent-active',
      end_status: 'success',
      response: 'done after denial',
    }),
  );
  assert(!subagentBuilder.hasActiveWork(), 'subagent turn end after denial clears activity');
}

function validateGeneratedMediaState() {
  const builder = new ConversationTimelineBuilder();
  builder.appendEvent(rawEvent('AssistantMessageStart', { msg_id: 1 }));
  builder.appendEvent(rawEvent('AssistantMessageChunk', { msg_id: 1, content: 'generating an image' }));
  builder.appendEvent(rawEvent('AssistantMessageEnd', { msg_id: 1, end_status: 'success' }));
  assert(!builder.hasActiveWork(), 'a finished assistant message leaves the conversation idle');

  builder.appendEvent(rawEvent('AssistantMediaGenerating', { media_id: 'img-1' }));
  assert(builder.timeline.length === 2, 'generated media renders in its own assistant row');
  assert(
    !builder.hasActiveWork(),
    'a generated media row never counts as active work because it has no message lifecycle',
  );

  builder.appendEvent(
    rawEvent('AssistantMediaOutput', {
      media_id: 'img-1',
      end_status: 'Succeeded',
      media: { relative_path: 'abc.png', media_type: 'image/png' },
    }),
  );
  assert(!builder.hasActiveWork(), 'a resolved media row leaves the conversation idle');
  const mediaRow = builder.timeline.at(-1);
  assert(mediaRow.kind === 'assistant', 'the generated image lands in an assistant row');
  const image = mediaRow.contentBlocks.find((block) => block.kind === 'image');
  assert(
    image && image.pending === false && image.image?.relative_path === 'abc.png',
    'the media output replaces the placeholder with the generated image',
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

function enterState(overrides = {}) {
  return {
    key: 'Enter',
    shiftKey: false,
    ctrlKey: false,
    altKey: false,
    metaKey: false,
    isComposing: false,
    ...overrides,
  };
}

function validateComposerEnterDecision() {
  assert(shouldSubmitOnEnter(false, enterState()) === true, 'desktop plain Enter submits');
  assert(shouldSubmitOnEnter(false, enterState({ key: 'a' })) === false, 'desktop non-Enter key does not submit');
  assert(shouldSubmitOnEnter(false, enterState({ shiftKey: true })) === false, 'desktop Shift+Enter is a newline');
  assert(shouldSubmitOnEnter(false, enterState({ ctrlKey: true })) === false, 'desktop Ctrl+Enter does not submit');
  assert(shouldSubmitOnEnter(false, enterState({ altKey: true })) === false, 'desktop Alt+Enter does not submit');
  assert(shouldSubmitOnEnter(false, enterState({ metaKey: true })) === false, 'desktop Meta+Enter does not submit');
  assert(shouldSubmitOnEnter(false, enterState({ isComposing: true })) === false, 'desktop IME composing Enter does not submit');

  assert(shouldSubmitOnEnter(true, enterState()) === false, 'touch plain Enter is a newline');
  assert(shouldSubmitOnEnter(true, enterState({ key: 'a' })) === false, 'touch non-Enter key does not submit');
  assert(shouldSubmitOnEnter(true, enterState({ shiftKey: true })) === false, 'touch Shift+Enter is a newline');
  assert(shouldSubmitOnEnter(true, enterState({ ctrlKey: true })) === false, 'touch Ctrl+Enter is a newline');
  assert(shouldSubmitOnEnter(true, enterState({ altKey: true })) === false, 'touch Alt+Enter is a newline');
  assert(shouldSubmitOnEnter(true, enterState({ metaKey: true })) === false, 'touch Meta+Enter is a newline');
  assert(shouldSubmitOnEnter(true, enterState({ isComposing: true })) === false, 'touch IME composing Enter is a newline');
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

function validateWireFormatNormalization() {
  const newShape = parseStreamLine(
    '{"id": 7, "msg": {"UserMessage": {"created_at": 123, "content": "hi", "media_filenames": []}}}',
  );
  assert(newShape?.wire?.variant === 'UserMessage', 'new shape parses the variant from the msg envelope');
  assert(newShape?.wire?.payload.msg_id === 7, 'new shape injects the envelope id into payload.msg_id');

  const legacyShape = parseStreamLine(
    '{"UserMessage": {"msg_id": 7, "created_at": 123, "content": "hi", "media_filenames": []}}',
  );
  assert(legacyShape?.wire?.variant === 'UserMessage', 'legacy shape parses the top-level variant key');
  assert(legacyShape?.wire?.payload.msg_id === 7, 'legacy shape keeps the payload msg_id');

  const noMsgIdVariant = parseStreamLine('{"id": 8, "msg": {"AggregateTokenUpdate": {}}}');
  assert(noMsgIdVariant?.wire?.variant === 'AggregateTokenUpdate', 'new shape parses variants that historically had no msg_id');
  assert(
    noMsgIdVariant?.wire?.payload.msg_id === 8,
    'new shape normalizes msg_id for variants that historically had none',
  );

  const multiVariant = parseStreamLine('{"id": 8, "msg": {"UserMessage": {}, "AssistantMessageStart": {}}}');
  assert(multiVariant?.wire === null, 'multi-variant msg envelopes fall back to raw events');

  const notAnEnvelope = parseStreamLine('{"id": 8, "msg": "not an object"}');
  assert(notAnEnvelope?.wire === null, 'non-object msg values fall back to raw events');
}

// Matches the math placeholder contract from markdown.ts exactly: a `span` element
// with the two data attributes and no children (an exactly-empty placeholder).
const mathPlaceholderPattern =
  /<span class="math-src" data-display="(block|inline)" data-tex="([^"]*)"><\/span>/g;

function decodeMathAttribute(value) {
  return value
    .replace(/&quot;/g, '"')
    .replace(/&gt;/g, '>')
    .replace(/&lt;/g, '<')
    .replace(/&amp;/g, '&');
}

function extractMathPlaceholders(html) {
  const matches = [...html.matchAll(mathPlaceholderPattern)];
  const openTags = (html.match(/<span class="math-src"/g) ?? []).length;
  assert(
    matches.length === openTags,
    `every math placeholder is an empty span element (open tags: ${openTags}, empty placeholders: ${matches.length})`,
  );
  return matches.map((match) => ({ display: match[1], tex: decodeMathAttribute(match[2]) }));
}

function validateMathRendering() {
  const motivating = `\\[
L=Z^{(N)}W_{\\text{vocab}}+b
\\qquad
L\\in\\mathbb R^{T\\times|\\mathcal V|}
\\]`;
  // The inner source sits between the `\[`/`\]` lines, so it keeps the surrounding newlines.
  const motivatingTex =
    '\nL=Z^{(N)}W_{\\text{vocab}}+b\n\\qquad\nL\\in\\mathbb R^{T\\times|\\mathcal V|}\n';
  const motivatingPlaceholders = extractMathPlaceholders(marked.parse(motivating));
  assert(
    motivatingPlaceholders.length === 1,
    'motivating display example produces exactly one placeholder',
  );
  assert(
    motivatingPlaceholders[0].display === 'block',
    'motivating display example is a block placeholder',
  );
  assert(
    motivatingPlaceholders[0].tex === motivatingTex,
    'motivating display example preserves the inner LaTeX source',
  );

  // A blank line ends the paragraph, so this case is only reachable via the block tokenizer.
  const blankLine = `\\[
L=Z^{(N)}W_{\\text{vocab}}+b

\\qquad
L\\in\\mathbb R^{T\\times|\\mathcal V|}
\\]`;
  const blankLinePlaceholders = extractMathPlaceholders(marked.parse(blankLine));
  assert(
    blankLinePlaceholders.length === 1,
    'display math spanning a blank line produces exactly one placeholder',
  );
  assert(
    blankLinePlaceholders[0].display === 'block',
    'display math spanning a blank line is a block placeholder',
  );

  const bracketOneLine = extractMathPlaceholders(marked.parse('\\[x^2\\]'));
  assert(bracketOneLine.length === 1, 'single-line \\[...\\] produces one placeholder');
  assert(bracketOneLine[0].display === 'block', 'single-line \\[...\\] is a block placeholder');
  assert(bracketOneLine[0].tex === 'x^2', 'single-line \\[...\\] keeps its LaTeX source');

  const doubleDollarOneLine = extractMathPlaceholders(marked.parse('$$\\frac{a}{b}$$'));
  assert(doubleDollarOneLine.length === 1, 'single-line $$...$$ produces one placeholder');
  assert(doubleDollarOneLine[0].display === 'block', 'single-line $$...$$ is a block placeholder');

  const parenInline = extractMathPlaceholders(marked.parse('inline \\(E=mc^2\\) here'));
  assert(parenInline.length === 1, 'inline \\(...\\) produces one placeholder');
  assert(parenInline[0].display === 'inline', 'inline \\(...\\) is an inline placeholder');
  assert(parenInline[0].tex === 'E=mc^2', 'inline \\(...\\) keeps its LaTeX source');

  const dollarInline = extractMathPlaceholders(marked.parse('costs $E=mc^2$ today'));
  assert(dollarInline.length === 1, 'inline $...$ produces one placeholder');
  assert(dollarInline[0].display === 'inline', 'inline $...$ is an inline placeholder');
  assert(dollarInline[0].tex === 'E=mc^2', 'inline $...$ keeps its LaTeX source');

  const inlineDisplay = extractMathPlaceholders(marked.parse('before \\[z\\] after'));
  assert(
    inlineDisplay.length === 1,
    'display delimiter inside a paragraph produces one placeholder',
  );
  assert(inlineDisplay[0].display === 'block', 'display delimiter inside a paragraph is block-mode');
  assert(inlineDisplay[0].tex === 'z', 'display delimiter inside a paragraph keeps its LaTeX source');

  const doubleDollarWins = extractMathPlaceholders(marked.parse('$$x$$'));
  assert(doubleDollarWins.length === 1, '$$x$$ produces exactly one placeholder');
  assert(doubleDollarWins[0].tex === 'x', '$$x$$ uses the $$-delimiter interpretation');

  const currency9 = marked.parse('It costs $5 and $10.');
  assert(
    extractMathPlaceholders(currency9).length === 0,
    'currency prose "$5 and $10" produces no placeholder',
  );
  assert(
    currency9.includes('$5') && currency9.includes('$10'),
    'currency prose "$5 and $10" keeps its literal dollar amounts',
  );

  const currency10 = marked.parse('$20,000 x $30,000');
  assert(
    extractMathPlaceholders(currency10).length === 0,
    'currency prose "$20,000 x $30,000" produces no placeholder',
  );
  assert(
    currency10.includes('$20,000') && currency10.includes('$30,000'),
    'currency prose "$20,000 x $30,000" keeps its literal amounts',
  );

  const escapedDollar = marked.parse('\\$5');
  assert(
    extractMathPlaceholders(escapedDollar).length === 0,
    'escaped dollar \\$5 produces no placeholder',
  );
  assert(escapedDollar.includes('$5'), 'escaped dollar \\$5 renders as a literal dollar sign');

  const spaceAfterOpen = marked.parse('a $ x$ b');
  assert(
    extractMathPlaceholders(spaceAfterOpen).length === 0,
    'whitespace after the opening $ produces no placeholder',
  );
  assert(spaceAfterOpen.includes('$ x$'), 'text around a whitespace-after-open $ survives');

  const spaceBeforeClose = marked.parse('a $x $ b');
  assert(
    extractMathPlaceholders(spaceBeforeClose).length === 0,
    'whitespace before the closing $ produces no placeholder',
  );
  assert(spaceBeforeClose.includes('$x $'), 'text around a whitespace-before-close $ survives');

  const digitAfterClose = marked.parse('a $x$5 b');
  assert(
    extractMathPlaceholders(digitAfterClose).length === 0,
    'digit after the closing $ produces no placeholder',
  );
  assert(digitAfterClose.includes('$x$5'), 'text around a digit-after-close $ survives');

  const unclosedOpen = marked.parse('a $x');
  assert(
    extractMathPlaceholders(unclosedOpen).length === 0,
    'unclosed opening $ produces no placeholder',
  );
  assert(unclosedOpen.includes('$x'), 'unclosed opening $ text survives');

  const unclosedClose = marked.parse('a x$');
  assert(
    extractMathPlaceholders(unclosedClose).length === 0,
    'unclosed closing $ produces no placeholder',
  );
  assert(unclosedClose.includes('x$'), 'unclosed closing $ text survives');

  let halfTyped;
  try {
    halfTyped = marked.parse('\\[ L=Z^{(N)}W_{\\text{vocab');
  } catch (error) {
    throw new Error(`half-typed display formula must not throw from marked.parse: ${error}`);
  }
  assert(
    extractMathPlaceholders(halfTyped).length === 0,
    'half-typed display formula produces no placeholder',
  );
  assert(
    halfTyped.includes('L=Z^{(N)}W_{\\text{vocab'),
    'half-typed display formula text survives',
  );

  const crossLine = marked.parse('a $x\ny$ b');
  assert(
    extractMathPlaceholders(crossLine).length === 0,
    '$ span crossing a newline produces no placeholder',
  );
  assert(crossLine.includes('x') && crossLine.includes('y$'), 'text around a newline-crossing $ survives');

  const fenced = marked.parse('```\n\\[x\\]\n$y$\n```');
  assert(
    extractMathPlaceholders(fenced).length === 0,
    'math inside a fenced code block stays literal',
  );
  assert(fenced.includes('<pre>') && fenced.includes('<code>'), 'fenced code block renders as pre/code');
  assert(
    fenced.includes('\\[x\\]') && fenced.includes('$y$'),
    'fenced code block preserves math text verbatim',
  );

  const inlineCode = marked.parse('`$x$` and `\\[y\\]`');
  assert(extractMathPlaceholders(inlineCode).length === 0, 'math inside inline code stays literal');
  assert(inlineCode.includes('<code>$x$</code>'), 'inline code preserves $x$ verbatim');
  assert(inlineCode.includes('<code>\\[y\\]</code>'), 'inline code preserves \\[y\\] verbatim');

  const ampHtml = marked.parse('\\[a & b\\]');
  assert(
    ampHtml.includes('data-tex="a &amp; b"'),
    'ampersand in LaTeX is escaped in the raw placeholder attribute',
  );
  const ampPlaceholders = extractMathPlaceholders(ampHtml);
  assert(
    ampPlaceholders.length === 1 && ampPlaceholders[0].tex === 'a & b',
    'ampersand in LaTeX unescapes back to the source text',
  );

  const quoteHtml = marked.parse('\\[a"b\\]');
  assert(
    quoteHtml.includes('data-tex="a&quot;b"'),
    'double quote in LaTeX is escaped in the raw placeholder attribute',
  );
  const quotePlaceholders = extractMathPlaceholders(quoteHtml);
  assert(
    quotePlaceholders.length === 1 && quotePlaceholders[0].tex === 'a"b',
    'double quote in LaTeX unescapes back to the source text',
  );

  const typesetCases = [
    ['motivating display', motivatingPlaceholders[0].tex, true],
    ['single-line display', bracketOneLine[0].tex, true],
    ['inline paren', parenInline[0].tex, false],
    ['inline dollar', dollarInline[0].tex, false],
  ];
  for (const [label, tex, displayMode] of typesetCases) {
    let rendered;
    try {
      rendered = katex.renderToString(tex, { displayMode, throwOnError: true });
    } catch (error) {
      throw new Error(`KaTeX could not typeset the ${label} formula: ${error}`);
    }
    assert(rendered.includes('class="katex"'), `KaTeX typesets the ${label} formula`);
  }

  let invalidThrew = false;
  try {
    katex.renderToString('\\frac{a', { throwOnError: true });
  } catch {
    invalidThrew = true;
  }
  assert(invalidThrew, 'KaTeX throws on invalid LaTeX so the plain-text fallback is reachable');
}

function validateMediaUrl() {
  assert(
    mediaUrl({ sessionId: 'sid-1' }, 'abc.png') === '/api/sessions/sid-1/media/abc.png',
    'media URL without a subagent points at the session media route',
  );
  assert(
    mediaUrl({ sessionId: 'sid-1', subagentId: 'sub-2' }, 'abc.png') ===
      '/api/sessions/sid-1/subagents/sub-2/media/abc.png',
    'media URL with a subagent points at the subagent media route',
  );
  assert(
    mediaUrl({ sessionId: 'sid-1', subagentId: '' }, 'abc.png') === '/api/sessions/sid-1/media/abc.png',
    'empty subagent id falls back to the session media route',
  );
  assert(
    mediaUrl({ sessionId: 'sid 1', subagentId: 'sub/2' }, 'my image.png') ===
      '/api/sessions/sid%201/subagents/sub%2F2/media/my%20image.png',
    'media URL percent-encodes its path components',
  );
}

validateTimelineStore();
validateWireFormatNormalization();
validateSubagentInputAggregation();
validateActiveWorkState();
validateGeneratedMediaState();
validateSpecialToolArgsPresentation();
validateSubagentPromptPreview();
validateComposerEnterDecision();
validateMediaUrl();
validateMathRendering();
await validateStreamEventBatcher();
console.log('frontend test passed');
