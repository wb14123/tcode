-- Pure-reducer test suite for the model layer of tcode.lua. No buffers, no
-- extmarks, no vim.*: every test drives `T.apply(model, event, envelope_id)`
-- with plain event tables and asserts the resulting model state and diff
-- (added / updated_all / updated_content with exact delta texts).
--
-- Each test starts from `local m = T.reset_model()` so the file-scope model is
-- rebound and assertions never leak across tests.

local function count_type(m, type_)
  local n = 0
  for _, el in ipairs(m.elements) do
    if el.type == type_ then n = n + 1 end
  end
  return n
end

local function last_of(m, type_)
  for i = #m.elements, 1, -1 do
    if m.elements[i].type == type_ then return m.elements[i] end
  end
  return nil
end

local function contains_entry(list, el)
  for _, e in ipairs(list) do
    if e == el then return true end
  end
  return false
end

local function has_delta(diff, el, text)
  for _, entry in ipairs(diff.updated_content) do
    if entry[1] == el and entry[2] == text then return true end
  end
  return false
end

-- ------------------------------------------------------------------ basics

test('reset_model: rebinds to a fresh, empty model', function()
  local m = T.reset_model()
  T.apply(m, { AssistantMessageStart = {} })
  check(#m.elements == 1, 'one element after an event')
  local m2 = T.reset_model()
  check(#m2.elements == 0, 'fresh model is empty')
  check(m2.sa_active == nil and m2.tail == nil and m2.pending_whitespace == nil, 'fresh model fields')
  check(m2.next_id == 0, 'fresh id counter')
  check(T.model == m2, 'T.model rebinds to the fresh model')
end)

test('shquote: wraps values so wire strings can never become shell syntax', function()
  check(T.shquote('plain') == "'plain'", 'plain string wrapped in single quotes')
  check(T.shquote("a'b") == "'a'\\''b'", "embedded quote escaped (')")
  check(T.shquote(42) == "'42'", 'numbers stringified')
  check(T.shquote('has spaces; $(touch /tmp/x) `rm -rf`') ==
    "'has spaces; $(touch /tmp/x) `rm -rf`'", 'shell metacharacters inert inside single quotes')
end)

test('UserMessage: added with content/media/created_at and envelope msg_id', function()
  local m = T.reset_model()
  local d = T.apply(m, { UserMessage = { content = 'hi there', media_filenames = { 'a.png' }, created_at = 1234 } }, 77)
  check(#d.added == 1 and #d.updated_all == 0 and #d.updated_content == 0, 'user message: only an added entry')
  local um = d.added[1]
  check(um.type == 'user_message', 'user message type')
  check(um.content == 'hi there', 'user message content')
  check(um.media_filenames and um.media_filenames[1] == 'a.png', 'user message media_filenames')
  check(um.created_at == 1234, 'user message created_at')
  check(um.msg_id == 77, 'envelope id captured as msg_id')
  check(m.tail == um, 'tail is the user message')
end)

test('UserMessage: falls back to data.msg_id and nil media', function()
  local m = T.reset_model()
  local d = T.apply(m, { UserMessage = { content = 'x', msg_id = 9 } })
  check(d.added[1].msg_id == 9, 'data.msg_id used when envelope id absent')
  check(d.added[1].media_filenames == nil, 'media_filenames nil when absent')
end)

test('AssistantMessageStart: adds an empty assistant message', function()
  local m = T.reset_model()
  local d = T.apply(m, { AssistantMessageStart = { created_at = 5 } })
  check(#d.added == 1, 'assistant start adds one')
  local am = d.added[1]
  check(am.type == 'assistant_message' and am.content == '', 'assistant message with empty content')
  check(am.created_at == 5, 'assistant message created_at')
  check(m.tail == am, 'tail is the assistant message')
end)

-- --------------------------------------------------------------- thinking

test('thinking: chunks append to the open block in exact order', function()
  local m = T.reset_model()
  local d1 = T.apply(m, { AssistantThinkingChunk = { content = 'A' } })
  local d2 = T.apply(m, { AssistantThinkingChunk = { content = 'B\nC' } })
  check(#m.elements == 1, 'single element for the stream')
  local t = m.elements[1]
  check(t.type == 'thinking_block' and t.state == 'open', 'block open')
  check(t.content == 'AB\nC', 'content accumulated in order')
  check(m.tail == t, 'tail is the block')
  check(contains_entry(d1.added, t), 'first chunk tagged added (rendered from state)')
  check(#d1.updated_content == 0, 'first chunk emits no content delta')
  check(has_delta(d2, t, 'B\nC'), 'second chunk delta exact')
end)

test('thinking: contiguous raw+summary chunks stay one entry', function()
  local m = T.reset_model()
  T.apply(m, { AssistantThinkingChunk = { content = 'raw part one' } })
  T.apply(m, { AssistantThinkingChunk = { content = ' raw part two' } })
  T.apply(m, { AssistantThinkingChunk = { content = '\nsummary bullet' } })
  check(#m.elements == 1, 'one entry for the whole reasoning stream')
  check(m.elements[1].content == 'raw part one raw part two\nsummary bullet', 'content concatenated in order')
  check(m.elements[1].state == 'open', 'still open')
end)

test('thinking: settle-flush collapse then reopen merges into one entry', function()
  local m = T.reset_model()
  T.apply(m, { AssistantThinkingChunk = { content = 'A1' } })
  local block = m.elements[1]
  local dc = T.close_open_elements(m) -- settle flush: tail stays on the block
  check(block.state == 'collapsed', 'open block collapsed by the settle flush')
  check(contains_entry(dc.updated_all, block), 'collapse tagged updated_all')
  check(m.tail == block, 'tail stays the block (nothing appended below)')
  -- Run 2 merges into the SAME element (no new block).
  local d2 = T.apply(m, { AssistantThinkingChunk = { content = 'B1' } })
  check(#m.elements == 1, 'no new thinking element created')
  check(m.elements[1] == block, 'same element id reused')
  check(block.state == 'open', 'block reopened')
  check(block.content == 'A1B1', 'content concatenated in order')
  check(#d2.added == 0, 'merge emits no added')
  check(contains_entry(d2.updated_all, block), 'merge tagged updated_all')
end)

test('thinking: collapse via a new element moves the tail, so no merge', function()
  local m = T.reset_model()
  T.apply(m, { AssistantThinkingChunk = { content = 'A1' } })
  local block = m.elements[1]
  local d = T.apply(m, { UserMessage = { content = 'next turn' } })
  check(block.state == 'collapsed', 'open block collapsed when user message opens')
  check(contains_entry(d.updated_all, block), 'collapse tagged updated_all')
  -- A later run starts a NEW block: the collapsed block is no longer the tail.
  local d2 = T.apply(m, { AssistantThinkingChunk = { content = 'B1' } })
  check(#m.elements == 3, 'run 2 starts a separate block')
  check(m.elements[3] ~= block and m.elements[3].content == 'B1', 'new block with its own content')
  check(block.content == 'A1', 'run 1 content untouched')
  check(contains_entry(d2.added, m.elements[3]), 'new block tagged added')
end)

test('thinking: an empty chunk while collapsed is a no-op (indicator preserved)', function()
  local m = T.reset_model()
  T.apply(m, { AssistantThinkingChunk = { content = 'A1' } })
  local block = m.elements[1]
  T.close_open_elements(m)
  check(block.state == 'collapsed', 'collapsed by the settle flush')
  local d = T.apply(m, { AssistantThinkingChunk = { content = '' } })
  check(#d.added == 0 and #d.updated_all == 0 and #d.updated_content == 0, 'empty chunk produces an empty diff')
  check(block.state == 'collapsed', 'block stays collapsed')
  check(block.content == 'A1', 'content untouched')
  check(m.tail == block, 'tail stays the collapsed block')
end)

test('thinking: an expanded block never merges a new run', function()
  local m = T.reset_model()
  T.apply(m, { AssistantThinkingChunk = { content = 'A' } })
  local block = m.elements[1]
  T.apply(m, { UserMessage = { content = 'x' } }) -- collapse
  check(block.state == 'collapsed', 'collapsed first')
  T.toggle_thinking_element(m, block)
  check(block.state == 'expanded', 'expanded by toggle')
  local d = T.apply(m, { AssistantThinkingChunk = { content = 'B' } })
  check(#m.elements == 3, 'new run starts a separate block')
  local b2 = m.elements[3]
  check(b2.type == 'thinking_block' and b2.content == 'B' and b2.state == 'open', 'separate open block')
  check(contains_entry(d.added, b2), 'new block tagged added')
  check(block.content == 'A', 'expanded block untouched')
end)

test('thinking: a run after visible text starts a separate block', function()
  local m = T.reset_model()
  T.apply(m, { AssistantMessageStart = {} })
  T.apply(m, { AssistantThinkingChunk = { content = 'A' } })
  local block = m.elements[2]
  T.apply(m, { AssistantMessageChunk = { content = 'visible' } }) -- collapses, text streams
  check(block.state == 'collapsed', 'collapsed by the text chunk')
  local d = T.apply(m, { AssistantThinkingChunk = { content = 'B' } })
  check(#m.elements == 3, 'separate block for run 2')
  local b2 = m.elements[3]
  check(b2.content == 'B' and b2.state == 'open', 'run 2 open with its own content')
  check(block.content == 'A', 'run 1 content untouched')
  check(contains_entry(d.added, b2), 'new block tagged added')
end)

-- ------------------------------------------------------------- whitespace

test('whitespace: held between collapsed runs, discarded on merge', function()
  local m = T.reset_model()
  T.apply(m, { AssistantThinkingChunk = { content = 'A1' } })
  local block = m.elements[1]
  T.close_open_elements(m) -- settle-flush collapse; tail stays on the block
  check(block.state == 'collapsed', 'collapsed first')
  local d = T.apply(m, { AssistantMessageChunk = { content = '\n\n' } })
  check(m.pending_whitespace == '\n\n', 'whitespace held in pending_whitespace')
  check(m.tail == block, 'tail stays the collapsed block')
  check(#d.updated_content == 0 and #d.added == 0 and #d.updated_all == 0, 'no diff entries for the whitespace')
  -- Next thinking chunk merges and DISCARDS the whitespace.
  local d2 = T.apply(m, { AssistantThinkingChunk = { content = 'B1' } })
  check(block.content == 'A1B1', 'merged content, whitespace discarded')
  check(m.pending_whitespace == nil, 'pending whitespace discarded')
  check(#m.elements == 1, 'still one thinking element')
  check(contains_entry(d2.updated_all, block), 'merge tagged updated_all')
end)

test('whitespace: flushed prepended to the next text chunk (coalesced delta)', function()
  local m = T.reset_model()
  T.apply(m, { AssistantMessageStart = {} })
  T.apply(m, { AssistantThinkingChunk = { content = 'A' } })
  T.apply(m, { UserMessage = { content = 'x' } }) -- collapse
  T.apply(m, { AssistantMessageChunk = { content = '\n' } })
  check(m.pending_whitespace == '\n', 'pending set')
  local am = last_of(m, 'assistant_message')
  local d = T.apply(m, { AssistantMessageChunk = { content = 'hello' } })
  check(am.content == '\nhello', 'pending prepended to the assistant message content')
  check(#d.updated_content == 1, 'one coalesced content entry')
  check(d.updated_content[1][1] == am and d.updated_content[1][2] == '\nhello', 'delta = whitespace..chunk')
  check(m.pending_whitespace == nil, 'pending flushed')
  check(m.tail == am, 'tail is assistant message after text')
end)

test('whitespace: flushed before a new element opens', function()
  local m = T.reset_model()
  T.apply(m, { AssistantMessageStart = {} })
  T.apply(m, { AssistantThinkingChunk = { content = 'A' } })
  T.apply(m, { UserMessage = { content = 'x' } })
  T.apply(m, { AssistantMessageChunk = { content = '\n' } })
  check(m.pending_whitespace == '\n', 'pending set')
  local am = last_of(m, 'assistant_message')
  local d = T.apply(m, { AssistantToolCallStart = { tool_call_id = 't1', tool_name = 'bash', tool_call_index = 0 } })
  check(am.content == '\n', 'whitespace flushed before the tool call opens')
  check(m.pending_whitespace == nil, 'pending cleared')
  check(has_delta(d, am, '\n'), 'flush delta tagged')
  check(#d.added == 1 and d.added[1].type == 'tool_call', 'tool call added after flush')
end)

test('whitespace: flushed at AssistantMessageEnd', function()
  local m = T.reset_model()
  T.apply(m, { AssistantMessageStart = {} })
  T.apply(m, { AssistantThinkingChunk = { content = 'A' } })
  T.apply(m, { UserMessage = { content = 'x' } })
  T.apply(m, { AssistantMessageChunk = { content = '\n' } })
  local am = last_of(m, 'assistant_message')
  local d = T.apply(m, { AssistantMessageEnd = { input_tokens = 1, output_tokens = 2 } })
  check(am.content == '\n', 'whitespace flushed at turn end')
  check(m.pending_whitespace == nil, 'pending cleared')
  check(has_delta(d, am, '\n'), 'flush delta tagged')
  check(m.tail.type == 'end_info', 'tail is end_info')
end)

test('whitespace: flushed at AssistantRequestEnd and UserMessage', function()
  local m = T.reset_model()
  T.apply(m, { AssistantMessageStart = {} })
  T.apply(m, { AssistantThinkingChunk = { content = 'A' } })
  T.apply(m, { UserMessage = { content = 'x' } })
  T.apply(m, { AssistantMessageChunk = { content = '\n' } })
  local am = last_of(m, 'assistant_message')
  T.apply(m, { AssistantRequestEnd = { total_input_tokens = 1, total_output_tokens = 2 } })
  check(am.content == '\n', 'flushed at assistant request end')
  check(m.pending_whitespace == nil, 'pending cleared after request end')

  local m2 = T.reset_model()
  T.apply(m2, { AssistantMessageStart = {} })
  T.apply(m2, { AssistantThinkingChunk = { content = 'A' } })
  T.apply(m2, { AssistantMessageChunk = { content = '\n' } }) -- collapses then holds
  local am2 = last_of(m2, 'assistant_message')
  T.apply(m2, { UserMessage = { content = 'next' } })
  check(am2.content == '\n', 'flushed before a new user turn')
  check(m2.pending_whitespace == nil, 'pending cleared after user message')
end)

test('whitespace: no phantom assistant message when none exists', function()
  -- A stray whitespace-only chunk flushed at UserMessage / AssistantRequestEnd
  -- must not materialize an empty '► ASSISTANT' block.
  local m = T.reset_model()
  T.apply(m, { AssistantMessageChunk = { content = '\n\n' } })
  check(m.pending_whitespace == '\n\n', 'whitespace held pending')
  local d = T.apply(m, { UserMessage = { content = 'next' } })
  check(count_type(m, 'assistant_message') == 0, 'no phantom assistant message created')
  check(#m.elements == 1 and m.elements[1].type == 'user_message', 'only the user message exists')
  check(#d.added == 1, 'only the user message added')
  check(m.pending_whitespace == nil, 'pending discarded')

  local m2 = T.reset_model()
  T.apply(m2, { AssistantMessageChunk = { content = '\n' } })
  T.apply(m2, { AssistantRequestEnd = { total_input_tokens = 1, total_output_tokens = 2 } })
  check(count_type(m2, 'assistant_message') == 0, 'no phantom block at AssistantRequestEnd')
  check(m2.tail.type == 'end_marker', 'tail is the end marker')
end)

-- ---------------------------------------------------- collapse-on-new-element

test('invariant: open thinking collapses before every new element variant', function()
  local variants = {
    { 'AssistantToolCallStart', { tool_call_id = 't1', tool_call_index = 0 } },
    { 'SystemMessage', { level = 'Info', message = 'sys' } },
    { 'AssistantMessageStart', {} },
    { 'LLMRetry', { attempt = 1, max_retries = 2, reason = 'r' } },
    { 'AssistantMediaOutput', { media = { relative_path = 'x.png' } } },
    { 'UserMessage', { content = 'u' } },
    { 'AssistantRequestEnd', { total_input_tokens = 1, total_output_tokens = 2 } },
    { 'AssistantMessageEnd', { input_tokens = 1, output_tokens = 2 } },
    { 'SubAgentInputStart', { tool_call_id = 'sa1', tool_call_index = 0 } },
    { 'ToolMessageStart', { tool_call_id = 't1', tool_name = 'bash', tool_args = '{}' } },
    { 'SubAgentStart', { tool_call_id = 'sa1', conversation_id = 'c1', description = 'd' } },
    { 'SubAgentContinue', { tool_call_id = 'sa2', conversation_id = 'c1', description = 'd' } },
    { 'SubAgentEnd', { conversation_id = 'c1', end_status = 'Succeeded' } },
  }
  for _, v in ipairs(variants) do
    local m = T.reset_model()
    T.apply(m, { AssistantThinkingChunk = { content = 'A' } })
    local block = m.elements[1]
    local d = T.apply(m, { [v[1]] = v[2] })
    check(block.state == 'collapsed', v[1] .. ': open thinking collapsed first')
    check(contains_entry(d.updated_all, block), v[1] .. ': collapse tagged updated_all')
  end
end)

-- -------------------------------------------------------- assistant message

test('message chunk: bare text with no assistant message creates one', function()
  local m = T.reset_model()
  local d = T.apply(m, { AssistantMessageChunk = { content = 'stray' } })
  check(#d.added == 1 and d.added[1].type == 'assistant_message', 'assistant message created defensively')
  check(d.added[1].content == 'stray', 'content on the new element')
  check(m.tail.type == 'assistant_message', 'tail is the assistant message')
end)

test('message chunk: sa_active set but no subagent element falls through', function()
  local m = T.reset_model()
  T.apply(m, { AssistantMessageStart = {} })
  m.sa_active = 'ghost' -- corrupt-state simulation
  local d = T.apply(m, { AssistantMessageChunk = { content = 'x' } })
  check(m.elements[1].content == 'x', 'appended to the assistant message')
  check(m.tail.type == 'assistant_message', 'tail is the assistant message')
  check(has_delta(d, m.elements[1], 'x'), 'delta tagged on the assistant message')
end)

test('message chunk: sa_active routes to the subagent, assistant message untouched', function()
  local m = T.reset_model()
  T.apply(m, { AssistantMessageStart = {} })
  local am = m.elements[1]
  T.apply(m, { SubAgentInputStart = { tool_call_id = 'sa1', tool_call_index = 0 } })
  T.apply(m, { AssistantMessageEnd = {} }) -- closes the input fence (real protocol)
  local sa = m.elements[2]
  T.apply(m, { SubAgentStart = { tool_call_id = 'sa1', conversation_id = 'c1', description = 'd' } })
  check(m.sa_active == 'c1', 'sa_active set')
  local d1 = T.apply(m, { AssistantMessageChunk = { content = 'out1' } })
  local d2 = T.apply(m, { AssistantMessageChunk = { content = ' out2' } })
  check(sa.output == 'out1 out2', 'chunks appended to the subagent output')
  check(has_delta(d1, sa, 'out1') and has_delta(d2, sa, ' out2'), 'deltas target the subagent element')
  check(am.content == '', 'assistant message content unchanged')
  check(m.tail == sa, 'tail stays the subagent (never moves to the assistant message)')
  check(count_type(m, 'assistant_message') == 1, 'no second assistant message created')
end)

-- ------------------------------------------------------------ tool lifecycle

test('tool call: full lifecycle with exact deltas', function()
  local m = T.reset_model()
  local d = T.apply(m, { AssistantToolCallStart = { tool_call_id = 't1', tool_name = 'bash', tool_call_index = 0, created_at = 42 } })
  check(#d.added == 1, 'start adds one')
  local tc = d.added[1]
  check(tc.type == 'tool_call', 'tool call type')
  check(tc.tool_call_id == 't1' and tc.tool_name == 'bash' and tc.tool_call_index == 0, 'identity fields')
  check(tc.args_open == true and tc.args_collapsed == false and tc.output_open == false, 'fence state at start')
  check(tc.status == 'generating' and tc.full_input == false, 'generating, not full input')
  check(tc.args == '' and tc.output == '', 'empty args/output')
  check(m.tail == tc, 'tail is the tool call')

  T.apply(m, { AssistantToolCallArgChunk = { tool_call_index = 0, content = '{"cmd":\n' } })
  local d3 = T.apply(m, { AssistantToolCallArgChunk = { tool_call_index = 0, content = '"ls",\n"extra"}' } })
  check(tc.args == '{"cmd":\n"ls",\n"extra"}', 'args accumulated')
  check(has_delta(d3, tc, '"ls",\n"extra"}'), 'second arg delta exact')

  local d4 = T.apply(m, { ToolMessageStart = { tool_call_id = 't1', tool_name = 'bash', tool_args = '' } })
  check(tc.args_open == false, 'args fence closed')
  check(tc.args_collapsed == true, '>2-line args collapsed')
  check(tc.status == 'running', 'status running')
  check(tc.output_open == true, 'output fence open')
  check(contains_entry(d4.updated_all, tc), 'tool message start tagged updated_all')
  check(m.tail == tc, 'tail stays the tool call')

  local d5 = T.apply(m, { ToolOutputChunk = { tool_call_id = 't1', content = 'out1' } })
  local d6 = T.apply(m, { ToolOutputChunk = { tool_call_id = 't1', content = '\nout2' } })
  check(tc.output == 'out1\nout2', 'output accumulated')
  check(has_delta(d5, tc, 'out1') and has_delta(d6, tc, '\nout2'), 'output deltas exact')

  local d7 = T.apply(m, { ToolMessageEnd = { tool_call_id = 't1', end_status = 'Succeeded', input_tokens = 3, output_tokens = 4 } })
  check(tc.output_open == false, 'output fence closed')
  check(tc.status == 'done', 'status done')
  check(contains_entry(d7.updated_all, tc), 'tool message end tagged updated_all')
  check(#d7.added == 1 and d7.added[1].type == 'end_info', 'end_info added')
  local info = d7.added[1]
  check(info.token_prefix == 'TOOL', 'TOOL token prefix on the end_info')
  check(info.tokens.input_tokens == 3 and info.tokens.output_tokens == 4, 'token fields')
  check(info.tokens.cache_creation_input_tokens == nil and info.tokens.cache_read_input_tokens == nil, 'no cache fields for tool end')
  check(m.tail == info, 'tail is end_info')
end)

test('tool call: single-line args are not collapsed', function()
  local m = T.reset_model()
  T.apply(m, { AssistantToolCallStart = { tool_call_id = 't', tool_call_index = 0 } })
  T.apply(m, { AssistantToolCallArgChunk = { tool_call_index = 0, content = '{"a":1}' } })
  local tc = m.elements[1]
  T.apply(m, { ToolMessageStart = { tool_call_id = 't', tool_args = '' } })
  check(tc.args_collapsed == false, '1-line args stay expanded')
end)

test('tool call: long single-line args (escaped JSON) collapse', function()
  -- Regression: read/edit/write args arrive as one logical line with escaped
  -- newlines; the old width-aware collapse previewed them, the line-count
  -- proxy did not. They must collapse again.
  local long = '{"content":"' .. string.rep('x', 400) .. '\\n\\nstill one line","path":"/tmp/f"}'
  local m = T.reset_model()
  T.apply(m, { AssistantToolCallStart = { tool_call_id = 't', tool_call_index = 0 } })
  T.apply(m, { AssistantToolCallArgChunk = { tool_call_index = 0, content = long } })
  local tc = m.elements[1]
  T.apply(m, { ToolMessageStart = { tool_call_id = 't', tool_args = '' } })
  check(tc.args_collapsed == true, 'long single-line args collapsed at ToolMessageStart')
  -- A moderately long single line (fits ~2 rows at the reference width) stays.
  local m2 = T.reset_model()
  T.apply(m2, { AssistantToolCallStart = { tool_call_id = 't', tool_call_index = 0 } })
  T.apply(m2, { AssistantToolCallArgChunk = { tool_call_index = 0, content = string.rep('y', 120) } })
  T.apply(m2, { ToolMessageStart = { tool_call_id = 't', tool_args = '' } })
  check(m2.elements[1].args_collapsed == false, 'short single-line args stay expanded')
end)

test('settle flush: long args/input collapse when the fence closes', function()
  -- Interrupted session: the file ends with an open args fence; the settle
  -- flush closes it and must collapse long content (old behavior).
  local long = '{"content":"' .. string.rep('z', 400) .. '"}'
  local m = T.reset_model()
  T.apply(m, { AssistantToolCallStart = { tool_call_id = 't', tool_call_index = 0 } })
  T.apply(m, { AssistantToolCallArgChunk = { tool_call_index = 0, content = long } })
  T.close_open_elements(m)
  local tc = m.elements[1]
  check(tc.args_open == false, 'fence closed by the flush')
  check(tc.args_collapsed == true, 'long args collapsed by the flush')
  -- Short pending args stay expanded.
  local m2 = T.reset_model()
  T.apply(m2, { AssistantToolCallStart = { tool_call_id = 't', tool_call_index = 0 } })
  T.apply(m2, { AssistantToolCallArgChunk = { tool_call_index = 0, content = 'a\nb' } })
  T.close_open_elements(m2)
  check(m2.elements[1].args_collapsed == false, 'short args stay expanded after the flush')
  -- Long pending subagent input collapses too.
  local m3 = T.reset_model()
  T.apply(m3, { SubAgentInputStart = { tool_call_id = 's', tool_call_index = 0 } })
  T.apply(m3, { SubAgentInputChunk = { tool_call_index = 0, content = long } })
  T.close_open_elements(m3)
  check(m3.elements[1].input_open == false and m3.elements[1].input_collapsed == true, 'long subagent input collapsed by the flush')
end)

test('subagent: long single-line input collapses at SubAgentStart', function()
  local long = '{"task":"' .. string.rep('w', 400) .. '"}'
  local m = T.reset_model()
  T.apply(m, { SubAgentInputStart = { tool_call_id = 's', tool_call_index = 0 } })
  T.apply(m, { SubAgentInputChunk = { tool_call_index = 0, content = long } })
  T.apply(m, { SubAgentStart = { tool_call_id = 's', conversation_id = 'c1', description = 'd' } })
  local sa = m.elements[1]
  check(sa.input_open == false and sa.input_collapsed == true, 'long single-line input collapsed at SubAgentStart')
end)

test('subagent: chunks after the settle flush still accumulate (regression)', function()
  -- The 500ms settle flush closes the input fence mid-stream; a pending
  -- subagent (conversation_id == nil) must keep accumulating later chunks or
  -- the input is silently truncated.
  local long = '{"task":"' .. string.rep('x', 400) .. '"'
  local m = T.reset_model()
  T.apply(m, { SubAgentInputStart = { tool_call_id = 'sa1', tool_call_index = 0 } })
  T.apply(m, { SubAgentInputChunk = { tool_call_index = 0, content = long } })
  local sa = m.elements[1]
  T.close_open_elements(m)
  check(sa.input_open == false, 'fence closed by the flush')
  check(sa.conversation_id == nil, 'still pending (no conversation id yet)')
  -- Chunks arriving after the pause must still accumulate into el.input.
  local d = T.apply(m, { SubAgentInputChunk = { tool_call_index = 0, content = ',"y":2}' } })
  check(sa.input == long .. ',"y":2}', 'chunk after the flush accumulated')
  check(has_delta(d, sa, ',"y":2}'), 'delta tagged on the element')
  -- SubAgentStart still transforms the element and collapses the long input.
  local ds = T.apply(m, { SubAgentStart = { tool_call_id = 'sa1', conversation_id = 'c1', description = 'd' } })
  check(sa.status == 'running' and sa.conversation_id == 'c1', 'start transforms the pending element')
  check(sa.input_open == false and sa.input_collapsed == true, 'long input collapsed at start')
  check(contains_entry(ds.updated_all, sa), 'start tagged updated_all')
  -- Once the conversation id is set, further chunks for the index are dropped.
  local d2 = T.apply(m, { SubAgentInputChunk = { tool_call_index = 0, content = 'stray' } })
  check(#d2.added == 0 and #d2.updated_all == 0 and #d2.updated_content == 0, 'post-start chunk dropped')
  check(sa.input == long .. ',"y":2}', 'input unchanged after start')
end)

test('tool call: full_input (detail view) never collapses long args', function()
  local m = T.reset_model()
  m.full_input = true -- what setup_tool_call_display sets on its model
  local d = T.apply(m, { AssistantToolCallStart = { tool_call_id = 't', tool_call_index = 0 } })
  check(d.added[1].full_input == true, 'tool_call element carries full_input')
  local tc = m.elements[1]
  T.apply(m, { AssistantToolCallArgChunk = { tool_call_index = 0, content = 'a\nb\nc\nd' } })
  check(tc.args == 'a\nb\nc\nd', 'args accumulated')
  T.apply(m, { ToolMessageStart = { tool_call_id = 't', tool_args = '' } })
  check(tc.args_collapsed == false, '>2-line args NOT collapsed when full_input is set')
  check(tc.status == 'running' and tc.output_open == true, 'fence/status transitions still apply')
  -- The same sequence WITHOUT full_input collapses (guards the rule).
  local m2 = T.reset_model()
  T.apply(m2, { AssistantToolCallStart = { tool_call_id = 't', tool_call_index = 0 } })
  T.apply(m2, { AssistantToolCallArgChunk = { tool_call_index = 0, content = 'a\nb\nc\nd' } })
  T.apply(m2, { ToolMessageStart = { tool_call_id = 't', tool_args = '' } })
  check(m2.elements[1].args_collapsed == true, 'same args collapse without full_input')
  check(m2.elements[1].full_input == false, 'default full_input is false')
end)

test('tool call: start defaults', function()
  local m = T.reset_model()
  local d = T.apply(m, { AssistantToolCallStart = {} })
  local tc = d.added[1]
  check(tc.tool_name == '' and tc.tool_call_index == 0, 'defaults applied')
end)

test('tool call: parallel calls stay separate with correct tails', function()
  local m = T.reset_model()
  T.apply(m, { AssistantToolCallStart = { tool_call_id = 't1', tool_name = 'bash', tool_call_index = 0 } })
  T.apply(m, { AssistantToolCallStart = { tool_call_id = 't2', tool_name = 'grep', tool_call_index = 1 } })
  check(#m.elements == 2, 'two tool call elements')
  local t1, t2 = m.elements[1], m.elements[2]
  check(m.tail == t2, 'tail is the latest tool call')
  T.apply(m, { AssistantToolCallArgChunk = { tool_call_index = 0, content = 'a1' } })
  T.apply(m, { AssistantToolCallArgChunk = { tool_call_index = 1, content = 'b1' } })
  check(t1.args == 'a1' and t2.args == 'b1', 'args routed by index')
  T.apply(m, { ToolMessageStart = { tool_call_id = 't2', tool_args = '' } })
  T.apply(m, { ToolMessageStart = { tool_call_id = 't1', tool_args = '' } })
  check(t1.status == 'running' and t2.status == 'running', 'both running')
  T.apply(m, { ToolOutputChunk = { tool_call_id = 't2', content = 'BB' } })
  T.apply(m, { ToolOutputChunk = { tool_call_id = 't1', content = 'AA' } })
  check(t1.output == 'AA' and t2.output == 'BB', 'output routed by id')
  T.apply(m, { ToolMessageEnd = { tool_call_id = 't1', end_status = 'Succeeded' } })
  T.apply(m, { ToolMessageEnd = { tool_call_id = 't2', end_status = 'Failed' } })
  check(t1.status == 'done' and t1.output_open == false, 't1 done, fence closed')
  check(t2.status == 'failed' and t2.output_open == false, 't2 failed, fence closed')
  check(m.tail.type == 'end_info', 'tail is the last end_info')
end)

test('tool call: end status map', function()
  local cases = {
    Succeeded = 'done', Failed = 'failed', Cancelled = 'cancelled',
    Timeout = 'failed', UserDenied = 'denied', UnknownStatus = 'done',
  }
  for status, expected in pairs(cases) do
    local m = T.reset_model()
    T.apply(m, { AssistantToolCallStart = { tool_call_id = 't', tool_call_index = 0 } })
    T.apply(m, { ToolMessageEnd = { tool_call_id = 't', end_status = status } })
    check(m.elements[1].status == expected, 'end_status ' .. tostring(status) .. ' -> ' .. expected)
  end
end)

test('tool end: every end_status (incl. nil) maps the status and adds a TOOL end_info', function()
  local cases = {
    { 'Succeeded', 'done' }, { 'Failed', 'failed' }, { 'Cancelled', 'cancelled' },
    { 'Timeout', 'failed' }, { 'UserDenied', 'denied' }, { nil, 'done' },
  }
  for _, c in ipairs(cases) do
    local m = T.reset_model()
    T.apply(m, { AssistantToolCallStart = { tool_call_id = 't', tool_call_index = 0 } })
    local tc = m.elements[1]
    local event = { ToolMessageEnd = { tool_call_id = 't', end_status = c[1] } }
    local d = T.apply(m, event)
    check(tc.status == c[2], 'end_status ' .. tostring(c[1]) .. ' -> element status ' .. c[2])
    check(#d.added == 1 and d.added[1].type == 'end_info', 'end_info added for ' .. tostring(c[1]))
    local info = d.added[1]
    check(info.token_prefix == 'TOOL', 'TOOL token prefix on the end_info')
    check(info.end_status == c[1], 'end_status carried on the end_info')
    check(m.tail == info, 'tail is the end_info')
  end
end)

test('tool call: fallback branch when no streamed args (resumed session)', function()
  local m = T.reset_model()
  local d = T.apply(m, { ToolMessageStart = { tool_call_id = 'fx', tool_name = 'edit', tool_args = '{"file":"a"}' } })
  check(#d.added == 1, 'fallback adds one')
  local tc = d.added[1]
  check(tc.type == 'tool_call' and tc.tool_call_id == 'fx', 'fallback tool call identity')
  check(tc.args == '{"file":"a"}', 'tool_args captured')
  check(tc.args_open == false and tc.output_open == true, 'fences: args closed, output open')
  check(tc.status == 'running', 'status running')
  check(m.tail == tc, 'tail is the fallback tool call')
  T.apply(m, { ToolOutputChunk = { tool_call_id = 'fx', content = 'res' } })
  check(tc.output == 'res', 'output routed to the fallback element')
  -- '{}' tool_args are treated as absent
  local m2 = T.reset_model()
  local d2 = T.apply(m2, { ToolMessageStart = { tool_call_id = 'fy', tool_name = 'edit', tool_args = '{}' } })
  check(d2.added[1].args == '', "'{}' args treated as absent")
end)

test('tool output: no element falls back to the tail assistant message', function()
  local m = T.reset_model()
  T.apply(m, { AssistantMessageStart = {} })
  local am = m.elements[1]
  local d = T.apply(m, { ToolOutputChunk = { tool_call_id = 'ghost', content = 'stray' } })
  check(am.content == 'stray', 'appended to the tail assistant message')
  check(has_delta(d, am, 'stray'), 'delta tagged on the assistant message')
  -- When the tail is not an assistant message -> no-op.
  local m2 = T.reset_model()
  T.apply(m2, { AssistantMessageStart = {} })
  T.apply(m2, { AssistantMessageEnd = {} })
  local d2 = T.apply(m2, { ToolOutputChunk = { tool_call_id = 'ghost', content = 'x' } })
  check(#d2.added == 0 and #d2.updated_all == 0 and #d2.updated_content == 0, 'no-op when tail is not assistant message')
end)

test('assistant message end: closes open tool args fences', function()
  local m = T.reset_model()
  T.apply(m, { AssistantToolCallStart = { tool_call_id = 't', tool_call_index = 0 } })
  local tc = m.elements[1]
  local d = T.apply(m, { AssistantMessageEnd = {} })
  check(tc.args_open == false, 'tool args fence closed')
  check(contains_entry(d.updated_all, tc), 'fence close tagged updated_all')
  check(m.tail.type == 'end_info', 'end_info added and is the tail')
end)

test('tool permission: request and approval status changes', function()
  local m = T.reset_model()
  T.apply(m, { AssistantToolCallStart = { tool_call_id = 't', tool_call_index = 0 } })
  local tc = m.elements[1]
  T.apply(m, { ToolRequestPermission = { tool_call_id = 't' } })
  check(tc.status == 'permission', 'permission status')
  T.apply(m, { ToolPermissionApproved = { tool_call_id = 't' } })
  check(tc.status == 'running', 'running after approval')
end)

test('tool permission: request/approval tag the element updated_all', function()
  local m = T.reset_model()
  T.apply(m, { AssistantToolCallStart = { tool_call_id = 't', tool_call_index = 0 } })
  local tc = m.elements[1]
  local d1 = T.apply(m, { ToolRequestPermission = { tool_call_id = 't' } })
  check(#d1.added == 0 and #d1.updated_content == 0, 'request emits no add/content entries')
  check(#d1.updated_all == 1 and contains_entry(d1.updated_all, tc), 'request tags the element updated_all')
  local d2 = T.apply(m, { ToolPermissionApproved = { tool_call_id = 't' } })
  check(tc.status == 'running', 'running after approval')
  check(#d2.updated_all == 1 and contains_entry(d2.updated_all, tc), 'approval tags the element updated_all')
  -- Unknown tool_call_id -> no-op, no status change.
  local m2 = T.reset_model()
  T.apply(m2, { AssistantToolCallStart = { tool_call_id = 't', tool_call_index = 0 } })
  local d3 = T.apply(m2, { ToolRequestPermission = { tool_call_id = 'ghost' } })
  check(#d3.added == 0 and #d3.updated_all == 0 and #d3.updated_content == 0, 'unknown id drops silently')
  check(m2.elements[1].status == 'generating', 'status unchanged for unknown id')
end)

-- ---------------------------------------------------------- subagent lifecycle

test('subagent: pending input transforms at Start, output streams to it', function()
  local m = T.reset_model()
  local d = T.apply(m, { SubAgentInputStart = { tool_call_id = 'sa1', tool_name = 'subagent', tool_call_index = 0, created_at = 1 } })
  check(#d.added == 1, 'input start adds one')
  local sa = d.added[1]
  check(sa.type == 'subagent' and sa.input_open == true, 'pending subagent with open input')
  check(sa.conversation_id == nil, 'no conversation id yet')
  check(sa.status == 'generating' and sa.is_continue == false, 'generating, not continue')
  check(m.tail == sa, 'tail is the pending subagent')

  T.apply(m, { SubAgentInputChunk = { tool_call_index = 0, content = '{"task":' } })
  T.apply(m, { SubAgentInputChunk = { tool_call_index = 0, content = '"do x"}' } })
  check(sa.input == '{"task":"do x"}', 'input accumulated')

  -- AssistantMessageEnd closes the input fence (as in the real protocol).
  local de = T.apply(m, { AssistantMessageEnd = {} })
  check(sa.input_open == false, 'input fence closed at AssistantMessageEnd')
  check(contains_entry(de.updated_all, sa), 'fence close tagged updated_all')

  -- SubAgentStart finds the pending element and transforms it in place.
  local ds = T.apply(m, { SubAgentStart = { tool_call_id = 'sa1', conversation_id = 'conv1', description = 'helper' } })
  check(#m.elements == 2, 'no new element: pending transformed')
  check(m.elements[1] == sa, 'same element, no duplicate')
  check(sa.status == 'running' and sa.description == 'helper', 'status and description set')
  check(sa.conversation_id == 'conv1', 'conversation id set')
  check(m.sa_active == 'conv1', 'sa_active set')
  check(contains_entry(ds.updated_all, sa), 'start tagged updated_all')

  -- Subagent output streams via AssistantMessageChunk onto the subagent.
  local am_count = count_type(m, 'assistant_message')
  local dc = T.apply(m, { AssistantMessageChunk = { content = 'result1' } })
  local dc2 = T.apply(m, { AssistantMessageChunk = { content = ' result2' } })
  check(sa.output == 'result1 result2', 'output appended to the subagent element')
  check(count_type(m, 'assistant_message') == am_count, 'no assistant message element created')
  check(has_delta(dc, sa, 'result1') and has_delta(dc2, sa, ' result2'), 'deltas on the subagent element')
end)

test('subagent: Continue transforms a pending input in place', function()
  local m = T.reset_model()
  T.apply(m, { SubAgentInputStart = { tool_call_id = 'sa2', tool_name = 'continue_subagent', tool_call_index = 1 } })
  local sa = m.elements[1]
  local d = T.apply(m, { SubAgentContinue = { tool_call_id = 'sa2', conversation_id = 'conv1', description = 'follow up' } })
  check(#m.elements == 1, 'pending element transformed, no new element')
  check(m.elements[1] == sa, 'same element')
  check(sa.status == 'continuing' and sa.is_continue == true, 'continuing with flag')
  check(sa.description == 'follow up', 'description updated')
  check(sa.conversation_id == 'conv1', 'conversation id set')
  check(sa.input_open == false, 'input fence closed')
  check(m.sa_active == 'conv1', 'sa_active set')
  check(contains_entry(d.updated_all, sa), 'continue tagged updated_all')
end)

test('subagent: Continue with no pending input adds a new continue element', function()
  local m = T.reset_model()
  T.apply(m, { SubAgentStart = { tool_call_id = 'sa1', conversation_id = 'conv1', description = 'first' } })
  local d = T.apply(m, { SubAgentContinue = { tool_call_id = 'sa9', conversation_id = 'conv1', description = 'second' } })
  check(#m.elements == 2, 'new continue element added')
  local cont = m.elements[2]
  check(cont.type == 'subagent' and cont.is_continue == true, 'continue element flagged')
  check(cont.status == 'continuing' and cont.conversation_id == 'conv1', 'continuing with conversation id')
  check(contains_entry(d.added, cont), 'tagged added')
  check(m.sa_active == 'conv1', 'sa_active set')
  -- Empty description inherits the last element of the conversation.
  local m2 = T.reset_model()
  T.apply(m2, { SubAgentStart = { tool_call_id = 'a', conversation_id = 'c', description = 'named' } })
  T.apply(m2, { SubAgentContinue = { tool_call_id = 'b', conversation_id = 'c', description = '' } })
  check(m2.elements[2].description == 'named', 'description falls back to the last element')
end)

test('subagent: End updates every conversation element and clears sa_active', function()
  local m = T.reset_model()
  T.apply(m, { SubAgentInputStart = { tool_call_id = 'sa1', tool_call_index = 0 } })
  T.apply(m, { SubAgentStart = { tool_call_id = 'sa1', conversation_id = 'conv1', description = 'a' } })
  T.apply(m, { SubAgentContinue = { tool_call_id = 'sa2', conversation_id = 'conv1', description = 'b' } })
  check(#m.elements == 2, 'two subagent elements')
  check(m.sa_active == 'conv1', 'sa_active set before end')
  local d = T.apply(m, { SubAgentEnd = { conversation_id = 'conv1', end_status = 'Succeeded', input_tokens = 5, output_tokens = 6 } })
  check(m.elements[1].status == 'done' and m.elements[2].status == 'done', 'both elements done')
  check(m.elements[1].input_tokens == 5 and m.elements[1].output_tokens == 6, 'tokens on the first element')
  check(m.elements[2].input_tokens == 5 and m.elements[2].output_tokens == 6, 'tokens on the last element')
  check(#d.updated_all == 2, 'both tagged updated_all')
  check(m.sa_active == nil, 'sa_active cleared')
  -- Non-Succeeded status is surfaced.
  local m2 = T.reset_model()
  T.apply(m2, { SubAgentStart = { tool_call_id = 'x', conversation_id = 'c2', description = 'd' } })
  T.apply(m2, { SubAgentEnd = { conversation_id = 'c2', end_status = 'Failed' } })
  check(m2.elements[1].status == 'Failed', 'failure status shown')
end)

test('subagent: End multi-entry update leaves the error on the last element', function()
  local m = T.reset_model()
  -- Section 1: SubAgentInputStart + SubAgentStart.
  T.apply(m, { SubAgentInputStart = { tool_call_id = 'sa1', tool_call_index = 0 } })
  T.apply(m, { SubAgentStart = { tool_call_id = 'sa1', conversation_id = 'c1', description = 'a' } })
  -- Section 2: SubAgentInputStart + SubAgentContinue (same conversation).
  T.apply(m, { SubAgentInputStart = { tool_call_id = 'sa2', tool_call_index = 1 } })
  T.apply(m, { SubAgentContinue = { tool_call_id = 'sa2', conversation_id = 'c1', description = 'b' } })
  check(#m.elements == 2, 'two subagent elements for the conversation')
  check(m.elements[2].is_continue == true, 'second element is the continue section')
  local d = T.apply(m, {
    SubAgentEnd = { conversation_id = 'c1', end_status = 'Failed', error = 'boom', input_tokens = 5, output_tokens = 6 },
  })
  check(m.elements[1].status == 'Failed' and m.elements[2].status == 'Failed',
    'BOTH elements get the final status')
  check(m.elements[1].input_tokens == 5 and m.elements[1].output_tokens == 6, 'tokens on the first element')
  check(m.elements[2].input_tokens == 5 and m.elements[2].output_tokens == 6, 'tokens on the last element')
  check(#d.updated_all == 2, 'both elements tagged updated_all')
  check(m.sa_active == nil, 'sa_active cleared')
  check(m.elements[1].error == nil, 'first element carries no error')
  check(m.elements[2].error == 'boom', 'LAST element carries the error text')
  -- Empty-string errors are not stored.
  local m2 = T.reset_model()
  T.apply(m2, { SubAgentStart = { tool_call_id = 'x', conversation_id = 'c2', description = 'd' } })
  T.apply(m2, { SubAgentEnd = { conversation_id = 'c2', end_status = 'Succeeded', error = '' } })
  check(m2.elements[1].status == 'done' and m2.elements[1].error == nil, 'empty error not stored')
end)

test('subagent: TurnEnd updates the last element and clears sa_active', function()
  local m = T.reset_model()
  T.apply(m, { SubAgentStart = { tool_call_id = 's', conversation_id = 'c', description = 'd' } })
  local d = T.apply(m, { SubAgentTurnEnd = { conversation_id = 'c', end_status = 'Succeeded', input_tokens = 1, output_tokens = 2 } })
  local sa = m.elements[1]
  check(sa.status == 'turn ended', 'turn ended status')
  check(sa.input_tokens == 1 and sa.output_tokens == 2, 'tokens stored for the label')
  check(contains_entry(d.updated_all, sa), 'tagged updated_all')
  check(m.sa_active == nil, 'sa_active cleared')
  local m2 = T.reset_model()
  T.apply(m2, { SubAgentStart = { tool_call_id = 's', conversation_id = 'c', description = 'd' } })
  T.apply(m2, { SubAgentTurnEnd = { conversation_id = 'c', end_status = 'Cancelled' } })
  check(m2.elements[1].status == 'Cancelled', 'failure status shown')
end)

test('subagent: permission status variants', function()
  local m = T.reset_model()
  T.apply(m, { SubAgentStart = { tool_call_id = 's', conversation_id = 'c', description = 'd' } })
  local sa = m.elements[1]
  T.apply(m, { SubAgentWaitingPermission = { conversation_id = 'c' } })
  check(sa.status == 'permission', 'waiting -> permission')
  T.apply(m, { SubAgentPermissionApproved = { conversation_id = 'c' } })
  check(sa.status == 'running', 'approved -> running')
  T.apply(m, { SubAgentWaitingPermission = { conversation_id = 'c' } })
  T.apply(m, { SubAgentPermissionDenied = { conversation_id = 'c' } })
  check(sa.status == 'running', 'denied -> running')
  -- The last element of the conversation is the target; continue sections
  -- restore to 'continuing'.
  T.apply(m, { SubAgentContinue = { tool_call_id = 's2', conversation_id = 'c', description = 'x' } })
  local cont = m.elements[2]
  check(cont.is_continue == true, 'continue element present')
  T.apply(m, { SubAgentWaitingPermission = { conversation_id = 'c' } })
  check(cont.status == 'permission', 'continue element waiting')
  T.apply(m, { SubAgentPermissionApproved = { conversation_id = 'c' } })
  check(cont.status == 'continuing', 'continue element restored to continuing')
end)

-- ------------------------------------------------------- misc element types

test('elements: end_info/end_marker/retry/media/system_message exact fields', function()
  local m = T.reset_model()
  T.apply(m, { AssistantMessageStart = {} })
  local d1 = T.apply(m, { AssistantMessageEnd = { end_status = 'Failed', error = 'boom', input_tokens = 1, output_tokens = 2, cache_creation_input_tokens = 3, cache_read_input_tokens = 4 } })
  local info = d1.added[1]
  check(info.type == 'end_info' and info.token_prefix == nil, 'end_info without prefix')
  check(info.tokens.input_tokens == 1 and info.tokens.output_tokens == 2, 'end_info token fields')
  check(info.tokens.cache_creation_input_tokens == 3 and info.tokens.cache_read_input_tokens == 4, 'end_info cache token fields')
  check(info.end_status == 'Failed' and info.error == 'boom', 'end_info status and error')

  local d2 = T.apply(m, { AssistantRequestEnd = { total_input_tokens = 10, total_output_tokens = 20, total_cache_creation_tokens = 30, total_cache_read_tokens = 40 } })
  local marker = d2.added[1]
  check(marker.type == 'end_marker', 'end_marker type')
  check(marker.tokens.total_input_tokens == 10 and marker.tokens.total_output_tokens == 20, 'end_marker totals')
  check(marker.tokens.total_cache_creation_tokens == 30 and marker.tokens.total_cache_read_tokens == 40, 'end_marker cache totals')
  check(m.tail == marker, 'tail is end_marker')

  local d3 = T.apply(m, { LLMRetry = { attempt = 2, max_retries = 3, reason = 'timeout' } })
  local retry = d3.added[1]
  check(retry.type == 'retry' and retry.attempt == 2 and retry.max_retries == 3 and retry.reason == 'timeout', 'retry fields')
  local m2 = T.reset_model()
  local d4 = T.apply(m2, { LLMRetry = {} })
  check(d4.added[1].attempt == 1 and d4.added[1].max_retries == 0 and d4.added[1].reason == '', 'retry defaults')

  local m3 = T.reset_model()
  local d5 = T.apply(m3, { AssistantMediaOutput = { media = { relative_path = 'uuid.png' } } })
  local media = d5.added[1]
  check(media.type == 'media' and media.relative_path == 'uuid.png', 'media fields')
  check(m3.tail == media, 'tail is media')
  local d6 = T.apply(m3, { AssistantMediaOutput = { media = nil, end_status = 'Failed' } })
  check(#d6.added == 0, 'failed media adds nothing')

  local m4 = T.reset_model()
  local d7 = T.apply(m4, { SystemMessage = { level = 'Warning', message = 'disk full' } })
  local sm = d7.added[1]
  check(sm.type == 'system_message' and sm.level == 'Warning' and sm.message == 'disk full', 'system message fields')
  local d8 = T.apply(m4, { SystemMessage = {} })
  check(d8.added[1].level == 'Info', 'system message default level')
end)

test('elements: field sweep across user_message/retry/media/end_marker/system_message', function()
  local m = T.reset_model()
  local d1 = T.apply(m, { UserMessage = { content = 'u', media_filenames = { 'a.png', 'b.png' }, created_at = 111 } })
  local um = d1.added[1]
  check(um.created_at == 111, 'user_message created_at')
  check(um.media_filenames[1] == 'a.png' and um.media_filenames[2] == 'b.png', 'user_message media_filenames list')
  check(um.content == 'u', 'user_message content')
  local d2 = T.apply(m, { AssistantMessageStart = {} })
  local d3 = T.apply(m, { LLMRetry = { attempt = 2, max_retries = 3, reason = 'timeout' } })
  local retry = d3.added[1]
  check(retry.attempt == 2 and retry.max_retries == 3 and retry.reason == 'timeout', 'retry attempt/max_retries/reason')
  local d4 = T.apply(m, { AssistantMediaOutput = { media = { relative_path = 'shot.png' } } })
  local media = d4.added[1]
  check(media.type == 'media' and media.relative_path == 'shot.png', 'media relative_path')
  local d5 = T.apply(m, { AssistantRequestEnd = { total_input_tokens = 1, total_output_tokens = 2, total_cache_creation_tokens = 3, total_cache_read_tokens = 4 } })
  local marker = d5.added[1]
  check(marker.type == 'end_marker' and marker.tokens.total_input_tokens == 1 and marker.tokens.total_output_tokens == 2,
    'end_marker tokens')
  check(marker.tokens.total_cache_creation_tokens == 3 and marker.tokens.total_cache_read_tokens == 4, 'end_marker cache tokens')
  local d6 = T.apply(m, { SystemMessage = { level = 'Error', message = 'fatal' } })
  local sm = d6.added[1]
  check(sm.level == 'Error' and sm.message == 'fatal', 'system_message level/message')
end)

-- ------------------------------------------------------------ no-op variants

test('no-ops: UserRequestEnd, PermissionUpdated, AssistantMediaGenerating, unknown', function()
  local m = T.reset_model()
  T.apply(m, { AssistantMessageStart = {} })
  local d1 = T.apply(m, { UserRequestEnd = { conversation_id = 'c' } })
  local d2 = T.apply(m, { PermissionUpdated = {} })
  local d3 = T.apply(m, { AssistantMediaGenerating = { media_id = 'm1' } })
  local d4 = T.apply(m, { TotallyUnknownVariant = { x = 1 } })
  for _, d in ipairs({ d1, d2, d3, d4 }) do
    check(#d.added == 0 and #d.updated_all == 0 and #d.updated_content == 0, 'empty diff for a no-op variant')
  end
  check(#m.elements == 1, 'model unchanged by no-ops')
end)

-- ------------------------------------------------------------ settle/toggles

test('close_open_elements: collapses open thinking', function()
  local m = T.reset_model()
  T.apply(m, { AssistantThinkingChunk = { content = 'A' } })
  local block = m.elements[1]
  local d = T.close_open_elements(m)
  check(block.state == 'collapsed', 'open thinking collapsed')
  check(#d.updated_all == 1 and contains_entry(d.updated_all, block), 'thinking tagged updated_all')
end)

test('close_open_elements: closes open args/input fences, idempotent', function()
  local m = T.reset_model()
  T.apply(m, { AssistantToolCallStart = { tool_call_id = 't', tool_call_index = 0 } })
  local tc = m.elements[1]
  T.apply(m, { SubAgentInputStart = { tool_call_id = 's', tool_call_index = 1 } })
  local sa = m.elements[2]
  local d = T.close_open_elements(m)
  check(tc.args_open == false and sa.input_open == false, 'both fences closed')
  check(#d.updated_all == 2, 'two updated_all entries')
  check(contains_entry(d.updated_all, tc) and contains_entry(d.updated_all, sa), 'both elements tagged')
  local d2 = T.close_open_elements(m)
  check(#d2.updated_all == 0 and #d2.added == 0 and #d2.updated_content == 0, 'second call is a no-op')
end)

test('toggles: thinking collapsed<->expanded, open blocks untouched', function()
  local m = T.reset_model()
  T.apply(m, { AssistantThinkingChunk = { content = 'A' } })
  local block = m.elements[1]
  T.apply(m, { UserMessage = { content = 'x' } }) -- collapse
  check(block.state == 'collapsed', 'collapsed first')
  local d = T.toggle_thinking_element(m, block)
  check(block.state == 'expanded', 'expanded by toggle')
  check(#d.updated_all == 1 and contains_entry(d.updated_all, block), 'expand tagged updated_all')
  local d2 = T.toggle_thinking_element(m, block)
  check(block.state == 'collapsed', 'collapsed by second toggle')
  -- Open blocks are not toggleable.
  local m2 = T.reset_model()
  T.apply(m2, { AssistantThinkingChunk = { content = 'B' } })
  local open_block = m2.elements[1]
  local d3 = T.toggle_thinking_element(m2, open_block)
  check(open_block.state == 'open', 'open block untouched')
  check(#d3.updated_all == 0, 'no diff for an open block')
end)

test('toggles: tool call args_collapsed flips', function()
  local m = T.reset_model()
  T.apply(m, { AssistantToolCallStart = { tool_call_id = 't', tool_call_index = 0 } })
  local tc = m.elements[1]
  T.apply(m, { ToolMessageStart = { tool_call_id = 't', tool_args = '' } })
  check(tc.args_collapsed == false, 'single-line args not collapsed by start')
  local d = T.toggle_tool_call_args_element(m, tc)
  check(tc.args_collapsed == true, 'collapsed by toggle')
  check(#d.updated_all == 1 and contains_entry(d.updated_all, tc), 'toggle tagged updated_all')
  local d2 = T.toggle_tool_call_args_element(m, tc)
  check(tc.args_collapsed == false, 'expanded by second toggle')
end)

test('tool end: long output auto-collapses, short stays expanded', function()
  local long = table.concat({ 'r1', 'r2', 'r3', 'r4', 'r5' }, '\n')
  local m = T.reset_model()
  T.apply(m, { AssistantToolCallStart = { tool_call_id = 't', tool_call_index = 0 } })
  T.apply(m, { ToolMessageStart = { tool_call_id = 't', tool_args = '' } })
  local tc = m.elements[1]
  T.apply(m, { ToolOutputChunk = { tool_call_id = 't', content = long } })
  check(tc.output_collapsed == false, 'streaming output is expanded')
  T.apply(m, { ToolMessageEnd = { tool_call_id = 't', end_status = 'Succeeded' } })
  check(tc.output_collapsed == true, 'long output collapsed at ToolMessageEnd')
  check(tc.output_open == false and tc.status == 'done', 'fence closed + status done')
  -- Short output stays expanded.
  local m2 = T.reset_model()
  T.apply(m2, { AssistantToolCallStart = { tool_call_id = 't', tool_call_index = 0 } })
  T.apply(m2, { ToolMessageStart = { tool_call_id = 't', tool_args = '' } })
  T.apply(m2, { ToolOutputChunk = { tool_call_id = 't', content = 'r1\nr2' } })
  T.apply(m2, { ToolMessageEnd = { tool_call_id = 't', end_status = 'Succeeded' } })
  check(m2.elements[1].output_collapsed == false, '2-line output stays expanded')
  -- full_input (detail view) never collapses the output.
  local m3 = T.reset_model()
  m3.full_input = true
  T.apply(m3, { AssistantToolCallStart = { tool_call_id = 't', tool_call_index = 0 } })
  T.apply(m3, { ToolMessageStart = { tool_call_id = 't', tool_args = '' } })
  T.apply(m3, { ToolOutputChunk = { tool_call_id = 't', content = long } })
  T.apply(m3, { ToolMessageEnd = { tool_call_id = 't', end_status = 'Succeeded' } })
  check(m3.elements[1].output_collapsed == false, 'full_input keeps the output expanded')
end)

test('subagent end: long output auto-collapses on the last element', function()
  local long = table.concat({ 'o1', 'o2', 'o3', 'o4' }, '\n')
  local m = T.reset_model()
  T.apply(m, { SubAgentInputStart = { tool_call_id = 's', tool_call_index = 0 } })
  T.apply(m, { SubAgentStart = { tool_call_id = 's', conversation_id = 'c1', description = 'd' } })
  local sa = m.elements[1]
  T.apply(m, { AssistantMessageChunk = { content = long } }) -- sa_active -> output
  check(sa.output_collapsed == false, 'streaming output expanded')
  T.apply(m, { SubAgentEnd = { conversation_id = 'c1', end_status = 'Succeeded' } })
  check(sa.output_collapsed == true, 'long output collapsed at SubAgentEnd')
  check(sa.status == 'done', 'final status set')
  -- Short output stays expanded.
  local m2 = T.reset_model()
  T.apply(m2, { SubAgentInputStart = { tool_call_id = 's', tool_call_index = 0 } })
  T.apply(m2, { SubAgentStart = { tool_call_id = 's', conversation_id = 'c1', description = 'd' } })
  T.apply(m2, { AssistantMessageChunk = { content = 'short' } })
  T.apply(m2, { SubAgentEnd = { conversation_id = 'c1', end_status = 'Succeeded' } })
  check(m2.elements[1].output_collapsed == false, 'short output stays expanded')
end)

test('toggles: tool/subagent output_collapsed flips', function()
  local m = T.reset_model()
  T.apply(m, { AssistantToolCallStart = { tool_call_id = 't', tool_call_index = 0 } })
  local tc = m.elements[1]
  local d = T.toggle_tool_output_element(m, tc)
  check(tc.output_collapsed == true, 'collapsed by toggle')
  check(#d.updated_all == 1 and contains_entry(d.updated_all, tc), 'toggle tagged updated_all')
  local d2 = T.toggle_tool_output_element(m, tc)
  check(tc.output_collapsed == false, 'expanded by second toggle')
  -- Subagent too.
  local m2 = T.reset_model()
  T.apply(m2, { SubAgentInputStart = { tool_call_id = 's', tool_call_index = 0 } })
  local sa = m2.elements[1]
  T.toggle_tool_output_element(m2, sa)
  check(sa.output_collapsed == true, 'subagent output collapses by toggle')
end)

-- ------------------------------------------------------------ dropped events

test('arg chunk: unknown tool_call_index drops silently', function()
  local m = T.reset_model()
  T.apply(m, { AssistantToolCallStart = { tool_call_id = 't', tool_call_index = 0 } })
  local d = T.apply(m, { AssistantToolCallArgChunk = { tool_call_index = 99, content = 'x' } })
  check(#d.added == 0 and #d.updated_all == 0 and #d.updated_content == 0, 'empty diff for unknown index')
  check(m.elements[1].args == '', 'args untouched')
end)

test('input chunk: unknown tool_call_index drops silently', function()
  local m = T.reset_model()
  T.apply(m, { SubAgentInputStart = { tool_call_id = 's', tool_call_index = 0 } })
  local d = T.apply(m, { SubAgentInputChunk = { tool_call_index = 99, content = 'x' } })
  check(#d.added == 0 and #d.updated_all == 0 and #d.updated_content == 0, 'empty diff for unknown index')
  check(m.elements[1].input == '', 'input untouched')
end)

-- ------------------------------------------------------------------ tail

test('tail: tracked across key sequences', function()
  local m = T.reset_model()
  T.apply(m, { AssistantThinkingChunk = { content = 'A' } })
  local block = m.elements[1]
  check(m.tail == block, 'open thinking -> tail is the block')
  T.apply(m, { UserMessage = { content = 'x' } })
  check(m.tail.type == 'user_message', 'user message -> tail is user message')
  T.apply(m, { AssistantMessageStart = {} })
  local am = m.elements[3]
  T.apply(m, { AssistantMessageChunk = { content = 'hi' } })
  check(m.tail == am, 'text chunk -> tail is assistant message')
  T.apply(m, { AssistantToolCallStart = { tool_call_id = 't', tool_call_index = 0 } })
  local tc = m.elements[4]
  check(m.tail == tc, 'tool call open -> tail is tool call')
  T.apply(m, { ToolMessageEnd = { tool_call_id = 't', end_status = 'Succeeded' } })
  check(m.tail.type == 'end_info', 'end_info -> tail is end_info')
  -- A collapsed block stays the tail through a whitespace chunk.
  local m2 = T.reset_model()
  T.apply(m2, { AssistantThinkingChunk = { content = 'A' } })
  local b2 = m2.elements[1]
  T.close_open_elements(m2)
  T.apply(m2, { AssistantMessageChunk = { content = '\n' } })
  check(m2.tail == b2, 'collapsed block -> tail stays the block through whitespace')
end)
