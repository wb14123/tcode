-- Buffer-level tests for the RENDERER layer of tcode.lua. Every test drives
-- `T.apply(model, event)` then `T.render(model, diff, ctx)` with a real scratch
-- buffer, and asserts the projected buffer rows, the integer row map, and the
-- modifiable invariant. The renderer is the ONLY layer that writes the buffer;
-- the diff contract comes from the reducer (added / updated_all /
-- updated_content). Element positions are plain integers in the row map, never
-- extmarks; extmarks carry only per-row highlight decoration.

local TC_FENCE = string.rep('`', 10)

-- Apply one event to the model and render the resulting diff into the buffer
-- (mirrors how the reader will call it: ONE event per render call).
-- ctx_extra optionally overrides render ctx fields (e.g. media_root).
local function apply_render(m, b, event, bulk, ctx_extra)
  local d = T.apply(m, event)
  local ctx = { buf = b, ns = ns, bulk = bulk or false }
  if ctx_extra then
    for k, v in pairs(ctx_extra) do ctx[k] = v end
  end
  T.render(m, d, ctx)
end

-- Render a reducer-level operation's diff (collapse/toggle) into the buffer.
local function render_diff(m, b, d)
  T.render(m, d, { buf = b, ns = ns, bulk = false })
end

-- The row-map entry for an element.
local function row_of(m, el)
  return T.get_renderer_state(m).rows[el.id]
end

-- Count the display-ns highlight marks of a group (per-row decoration).
local function count_hl(b, group)
  local n = 0
  local marks = vim.api.nvim_buf_get_extmarks(b, ns, 0, -1, { details = true })
  for _, mm in ipairs(marks) do
    if mm[4] and mm[4].hl_group == group then n = n + 1 end
  end
  return n
end

-- ------------------------------------------------------------------ basics

test('first_event: the first added replaces the initial empty row', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantMessageStart = {} })
  local am = m.elements[1]
  local l = lines_of(b)
  check(l[1] == '► ASSISTANT' and #l == 1, 'label replaces row 0, no trailing blank')
  local r = row_of(m, am)
  check(r.start_row == 0 and r.height == 1, 'row map: assistant at [0, 1)')
  -- Subsequent adds append at the tail; content inserts after the label.
  apply_render(m, b, { AssistantMessageChunk = { content = 'hi' } })
  l = lines_of(b)
  check(l[1] == '► ASSISTANT' and l[2] == 'hi', 'first chunk inserts after the label')
  check(row_of(m, am).height == 2, 'assistant height grew to 2')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('added: user_message renders label + content', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  local ts = os.date('%H:%M:%S', math.floor(1000 / 1000))
  apply_render(m, b, { UserMessage = { content = 'hello\nworld', created_at = 1000 } })
  local um = m.elements[1]
  local l = lines_of(b)
  check(l[1] == '► USER  ' .. ts and l[2] == 'hello' and l[3] == 'world', 'user rows: label + content')
  local r = row_of(m, um)
  check(r.start_row == 0 and r.height == 3, 'row map: user at [0, 3)')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('added: assistant_message renders the label only (no trailing blank)', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  local ts = os.date('%H:%M:%S', math.floor(5 / 1000))
  apply_render(m, b, { AssistantMessageStart = { created_at = 5 } })
  local l = lines_of(b)
  check(l[1] == '► ASSISTANT  ' .. ts and #l == 1, 'label row only, no trailing blank')
end)

test('added: system_message rows are highlighted per level', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { SystemMessage = { level = 'Warning', message = 'disk full' } })
  local l = lines_of(b)
  check(l[1] == '► SYSTEM [Warning]' and l[2] == 'disk full', 'system rows')
  local marks = vim.api.nvim_buf_get_extmarks(b, ns, 0, -1, { details = true })
  local warned = false
  for _, mm in ipairs(marks) do
    if mm[4] and mm[4].hl_group == 'TCodeSystemWarning' and mm[2] == 1 then warned = true end
  end
  check(warned, 'message row highlighted TCodeSystemWarning')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('added: retry renders the retry line with TCodeTokens highlight', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { LLMRetry = { attempt = 2, max_retries = 3, reason = 'timeout' } })
  local l = lines_of(b)
  check(l[1] == '► [Retrying... (attempt 2/3) -- timeout]', 'retry row text')
  local marks = vim.api.nvim_buf_get_extmarks(b, ns, 0, -1, { details = true })
  local hl = false
  for _, mm in ipairs(marks) do
    if mm[4] and mm[4].hl_group == 'TCodeTokens' and mm[2] == 0 then hl = true end
  end
  check(hl, 'retry row highlighted TCodeTokens')
end)

test('added: media renders blank + markdown image link from the media root', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  local media_root = vim.uri_encode(tmp_dir .. '/session/media/')
  apply_render(m, b, { AssistantMediaOutput = { media = { relative_path = 'uuid.png' } } },
    false, { media_root = media_root })
  local l = lines_of(b)
  check(l[1] == '' and l[2] == '![img](file://' .. media_root .. 'uuid.png)', 'blank + encoded image link rows')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('added: media without a media root is skipped entirely', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantMessageStart = {} })
  apply_render(m, b, { AssistantMediaOutput = { media = { relative_path = 'x.png' } } })
  local media = m.elements[2]
  local l = lines_of(b)
  check(#l == 1 and l[1] == '► ASSISTANT', 'media adds nothing without a media root')
  local r = row_of(m, media)
  check(r.start_row == nil and r.height == 0, 'zero-height media entry in the row map')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('added: end_info renders the token line and error rows', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantMessageStart = {} })
  apply_render(m, b, { AssistantMessageEnd = {
    end_status = 'Failed', error = 'boom',
    input_tokens = 1, output_tokens = 2,
    cache_creation_input_tokens = 0, cache_read_input_tokens = 4,
  } })
  local l = lines_of(b)
  check(l[1] == '► ASSISTANT' and l[2] == '► [1 in / 4 cache read / 2 out tokens] [Failed]'
    and l[3] == 'Error: boom', "rows: label, token+status line, error")
  local marks = vim.api.nvim_buf_get_extmarks(b, ns, 0, -1, { details = true })
  local error_hl = false
  for _, mm in ipairs(marks) do
    if mm[4] and mm[4].hl_group == 'TCodeError' and mm[2] == 2 then error_hl = true end
  end
  check(error_hl, 'error row highlighted TCodeError')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('added: end_info with nothing to show is skipped', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantMessageStart = {} })
  apply_render(m, b, { AssistantMessageEnd = {} })
  local info = m.elements[2]
  local l = lines_of(b)
  check(#l == 1 and l[1] == '► ASSISTANT', 'empty end_info writes no rows')
  local r = row_of(m, info)
  check(r.start_row == nil and r.height == 0, 'zero-height end_info entry in the row map')
end)

test('added: end_marker renders the total token line as real text', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantRequestEnd = {
    total_input_tokens = 10, total_output_tokens = 20,
    total_cache_creation_tokens = 30, total_cache_read_tokens = 40,
  } })
  local l = lines_of(b)
  check(l[1] == '► [Total: 40 in / 40 cache read / 20 out tokens]', 'total token row')
end)

-- --------------------------------------------------------------- thinking

test('thinking: live stream, collapse, expand, collapse roundtrip', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantMessageStart = {} })
  apply_render(m, b, { AssistantThinkingChunk = { content = 'A1\nA2' } })
  local block = m.elements[2]
  local l = lines_of(b)
  check(l[1] == '► ASSISTANT' and l[2] == 'A1' and l[3] == 'A2', 'content streams after the label')
  local r = row_of(m, block)
  check(r.start_row == 1 and r.height == 2, 'row map: block at [1, 3)')

  -- Collapse: one real hint row.
  render_diff(m, b, T.close_open_elements(m))
  l = lines_of(b)
  check(#l == 2 and l[2] == '► [Thinking... press o to expand]', 'collapsed to one real hint row')
  check(row_of(m, block).height == 1, 'collapsed height is 1')

  -- Expand: hint + full content.
  render_diff(m, b, T.toggle_thinking_element(m, block))
  l = lines_of(b)
  check(l[2] == '► [Thinking... press o to collapse]' and l[3] == 'A1' and l[4] == 'A2',
    'expanded content restored')
  check(row_of(m, block).height == 3, 'expanded height is 3')

  -- The collapsed row and the expanded content both resolve to a thinking
  -- toggle via the row map.
  render_diff(m, b, T.toggle_thinking_element(m, block))
  local el, off = T.element_at_row_full(m, b, 1)
  check(el == block and off == 0, 'collapsed row -> (block, offset 0)')
  check(T.action_at(block, 0) == 'thinking', 'collapsed row toggles thinking')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('thinking: an empty chunk does not erase the collapsed indicator', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantMessageStart = {} })
  apply_render(m, b, { AssistantThinkingChunk = { content = 'A1\nA2' } })
  local block = m.elements[2]
  render_diff(m, b, T.close_open_elements(m))
  local before = lines_of(b)
  check(before[2] == '► [Thinking... press o to expand]', 'collapsed indicator row')
  -- An empty thinking chunk (e.g. a burst boundary) must not reopen the block
  -- or replace the indicator row.
  apply_render(m, b, { AssistantThinkingChunk = { content = '' } })
  local after = lines_of(b)
  check(table.concat(after, '|') == table.concat(before, '|'), 'buffer unchanged by the empty chunk')
  check(block.state == 'collapsed', 'block stays collapsed')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('merge: a reopened block renders the FULL merged content', function()
  -- The merge-reopen renders full content (the old tail-only render lost
  -- previous runs — bug 1 root cause C).
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantMessageStart = {} })
  apply_render(m, b, { AssistantThinkingChunk = { content = 'A1\nA2' } })
  local block = m.elements[2]
  render_diff(m, b, T.close_open_elements(m))
  -- Run 2 arrives after the pause: the merge reopen must render the FULL
  -- content, previous run included.
  apply_render(m, b, { AssistantThinkingChunk = { content = 'B1' } })
  apply_render(m, b, { AssistantThinkingChunk = { content = '\nB2' } })
  local l = lines_of(b)
  check(l[1] == '► ASSISTANT' and l[2] == 'A1' and l[3] == 'A2B1' and l[4] == 'B2',
    "full merged content ['label','A1','A2B1','B2']")
  check(T.content_of(block, 'content') == 'A1\nA2B1\nB2', 'the model holds the full merged content')
  check(row_of(m, block).height == 3, 'merged block height matches the content rows')
  -- Final collapse yields one hint row.
  render_diff(m, b, T.close_open_elements(m))
  l = lines_of(b)
  check(l[2] == '► [Thinking... press o to expand]', 'single hint row after final collapse')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('merge: bulk runs separated by a whitespace chunk merge into one', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantMessageStart = {} }, true)
  apply_render(m, b, { AssistantThinkingChunk = { content = 'bulk one' } }, true)
  apply_render(m, b, { AssistantMessageChunk = { content = '\n' } }, true)
  apply_render(m, b, { AssistantThinkingChunk = { content = 'bulk two' } }, true)
  local block = m.elements[2]
  check(block.content == 'bulk onebulk two', 'model holds both bulk runs in order')
  local l = lines_of(b)
  check(l[1] == '► ASSISTANT' and l[2] == 'bulk onebulk two', 'merge reopen renders the full merged content')
  render_diff(m, b, T.close_open_elements(m))
  l = lines_of(b)
  check(l[2] == '► [Thinking... press o to expand]', 'merged run collapsed to one hint row')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

-- ---------------------------------------------------------- tool call

test('tool_call: full lifecycle with args/result sections and row-map shifts', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantToolCallStart = { tool_call_id = 't1', tool_name = 'bash', tool_call_index = 0 } })
  local tc = m.elements[1]
  local l = lines_of(b)
  check(l[1] == '► TOOL: [generating] bash  [Ctrl-k to cancel]' and #l == 1,
    'label only while args are still empty')
  check(row_of(m, tc).height == 1, 'initial tool height 1')

  apply_render(m, b, { AssistantToolCallArgChunk = { tool_call_index = 0, content = 'a1\nb2' } })
  apply_render(m, b, { AssistantToolCallArgChunk = { tool_call_index = 0, content = '\nb3\nb4' } })
  l = lines_of(b)
  check(l[2] == '► Param' and l[3] == TC_FENCE and l[4] == 'a1' and l[5] == 'b2' and l[6] == 'b3' and l[7] == 'b4',
    'args stream inside the Param fence (open, no close fence)')
  check(row_of(m, tc).height == 7, 'streamed args grew the tool to 7 rows')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')

  -- ToolMessageStart: args fence closes, Result section opens with an empty row.
  apply_render(m, b, { ToolMessageStart = { tool_call_id = 't1', tool_args = '' } })
  check(tc.output_started == true, 'output_started set')
  l = lines_of(b)
  check(l[8] == TC_FENCE and l[9] == '► Result' and l[10] == TC_FENCE and l[11] == '',
    'args close fence + Result header + open fence + empty output row')
  check(row_of(m, tc).height == 11, 'tool height 11 with the Result section')

  -- Output streams onto the tool's own rows.
  apply_render(m, b, { ToolOutputChunk = { tool_call_id = 't1', content = 'out1' } })
  apply_render(m, b, { ToolOutputChunk = { tool_call_id = 't1', content = '\nout2' } })
  l = lines_of(b)
  check(l[11] == 'out1' and l[12] == 'out2', 'output streams inside the Result fence')
  check(row_of(m, tc).height == 12, 'streamed output grew the tool to 12 rows')

  -- ToolMessageEnd: output fence closes, end_info added below the region.
  apply_render(m, b, { ToolMessageEnd = { tool_call_id = 't1', end_status = 'Succeeded', input_tokens = 3, output_tokens = 4 } })
  l = lines_of(b)
  check(l[13] == TC_FENCE, 'output close fence')
  check(l[1] == '► TOOL: [done] bash', 'label rebuilt with the done status')
  check(l[14] == '► [TOOL: 3 in / 4 out tokens]', 'end_info row after the region')
  check(row_of(m, tc).height == 13, 'closed tool height 13')
  local info = m.elements[2]
  check(row_of(m, info).start_row == 13 and row_of(m, info).height == 1, 'end_info at [13, 14)')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('tool_call: multi-line args render fully inside the Param fence', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantToolCallStart = { tool_call_id = 't1', tool_name = 'bash', tool_call_index = 0 } })
  apply_render(m, b, { AssistantToolCallArgChunk = { tool_call_index = 0, content = 'a\nb\nc\nd' } })
  apply_render(m, b, { ToolMessageStart = { tool_call_id = 't1', tool_args = '' } })
  local l = lines_of(b)
  check(l[4] == 'a' and l[5] == 'b' and l[6] == 'c' and l[7] == 'd',
    'args render as real content rows inside the Param fence')
  local hint_rows = 0
  for _, line in ipairs(l) do
    if line:find('press o to', 1, true) then hint_rows = hint_rows + 1 end
  end
  check(hint_rows == 0, 'no expand hint row exists for the tool')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('bulk: the initial load full-projects; later bulk renders are incremental', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  -- First bulk render: one full projection of the model.
  apply_render(m, b, { AssistantToolCallStart = { tool_call_id = 't1', tool_name = 'bash', tool_call_index = 0 } }, true)
  local l = lines_of(b)
  check(l[1] == '► TOOL: [generating] bash  [Ctrl-k to cancel]' and #l == 1,
    'label-only tool rendered at the full projection')
  check(vim.bo[b].modifiable == false, 'modifiable false after the full projection')

  -- A later bulk render is incremental: the args chunk streams normally.
  apply_render(m, b, { AssistantToolCallArgChunk = { tool_call_index = 0, content = 'a\nb\nc\nd' } }, true)
  l = lines_of(b)
  check(l[4] == 'a' and l[5] == 'b' and l[6] == 'c' and l[7] == 'd', 'args streamed incrementally')
  check(vim.bo[b].modifiable == false, 'modifiable false after the incremental chunk')

  -- ToolMessageStart rebuild closes the args fence and opens the Result section.
  apply_render(m, b, { ToolMessageStart = { tool_call_id = 't1', tool_args = '' } }, true)
  l = lines_of(b)
  check(l[8] == TC_FENCE and l[9] == '► Result' and l[10] == TC_FENCE and l[11] == '',
    'rebuild closes the args fence and opens the Result section')
  check(vim.bo[b].modifiable == false, 'modifiable false after the rebuild')
end)

test('parallel: tc1 output lands mid-buffer above tc2, later rows shift', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantToolCallStart = { tool_call_id = 'tc1', tool_name = 'bash', tool_call_index = 0 } })
  local tc1 = m.elements[1]
  apply_render(m, b, { AssistantToolCallStart = { tool_call_id = 'tc2', tool_name = 'grep', tool_call_index = 1 } })
  local tc2 = m.elements[2]
  apply_render(m, b, { ToolMessageStart = { tool_call_id = 'tc1', tool_args = '' } })
  apply_render(m, b, { ToolMessageStart = { tool_call_id = 'tc2', tool_args = '' } })
  local l = lines_of(b)
  -- Each tool: label + Result header + open fence + empty output row.
  check(#l == 8, 'two 4-row regions')
  check(row_of(m, tc1).start_row == 0 and row_of(m, tc2).start_row == 4, 'tc1 at 0, tc2 at 4')
  -- tc1 output streams into its own mid-buffer region.
  apply_render(m, b, { ToolOutputChunk = { tool_call_id = 'tc1', content = 'out1' } })
  apply_render(m, b, { ToolOutputChunk = { tool_call_id = 'tc1', content = '\nout2' } })
  l = lines_of(b)
  check(l[4] == 'out1' and l[5] == 'out2', 'tc1 output above tc2')
  check(l[6] == '► TOOL: [running] grep  [Ctrl-k to cancel]', 'tc2 label below tc1 output')
  check(row_of(m, tc1).height == 5 and row_of(m, tc2).start_row == 5, 'tc1 grew, tc2 shifted down')
  local el, off = T.element_at_row_full(m, b, 5)
  check(el == tc2 and off == 0, 'row 5 resolves to tc2 after the shift')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

-- -------------------------------------------------------------- subagent

test('subagent: input fence, output stream, final status and error rows', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { SubAgentInputStart = { tool_call_id = 'sa1', tool_name = 'subagent', tool_call_index = 0 } })
  local sa = m.elements[1]
  local l = lines_of(b)
  check(l[1] == '► SUB-AGENT: [generating]' and #l == 1, 'label only while input is empty')

  apply_render(m, b, { SubAgentInputChunk = { tool_call_index = 0, content = '{"task":' } })
  apply_render(m, b, { SubAgentInputChunk = { tool_call_index = 0, content = '"do x"}' } })
  l = lines_of(b)
  check(l[2] == '► Input' and l[3] == TC_FENCE and l[4] == '{"task":"do x"}',
    'input streams inside the Input fence (open)')

  -- AssistantMessageEnd closes the input fence and opens the Output section.
  apply_render(m, b, { AssistantMessageEnd = {} })
  l = lines_of(b)
  check(l[1] == '► SUB-AGENT: [generating]' and l[2] == '► Input'
    and l[3] == TC_FENCE and l[4] == '{"task":"do x"}' and l[5] == TC_FENCE and l[6] == '► Output'
    and l[7] == TC_FENCE and l[8] == '' and l[9] == TC_FENCE,
    'input fence closed + Output section (open fence + empty row + close fence)')

  -- SubAgentStart: running status, output region follows.
  apply_render(m, b, { SubAgentStart = { tool_call_id = 'sa1', conversation_id = 'conv1', description = 'helper' } })
  check(sa.status == 'running' and sa.conversation_id == 'conv1', 'status + conversation set')
  l = lines_of(b)
  check(l[1] == '► SUB-AGENT: [running]  helper', 'label rebuilt with running status + description')
  -- Subagent output streams via AssistantMessageChunk (sa_active).
  apply_render(m, b, { AssistantMessageChunk = { content = 'result1' } })
  apply_render(m, b, { AssistantMessageChunk = { content = ' result2' } })
  l = lines_of(b)
  check(l[8] == 'result1 result2', 'output streams inside the Output fence (no close fence)')

  -- SubAgentEnd: final status + error rows + the close fence.
  apply_render(m, b, { SubAgentEnd = { conversation_id = 'conv1', end_status = 'Failed', error = 'boom', input_tokens = 5, output_tokens = 6 } })
  l = lines_of(b)
  check(l[1] == '► SUB-AGENT: [Failed]  [5 in / 6 out]  helper', 'label rebuilt with the final status')
  check(l[9] == '' and l[10] == 'Error: boom', 'error rows at the region bottom')
  check(l[11] == TC_FENCE, 'output close fence after SubAgentEnd')
  check(row_of(m, sa).height == 11, 'subagent region height 11')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('subagent: a long input shows its capped tail inside the Input fence at SubAgentStart', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { SubAgentInputStart = { tool_call_id = 'sa1', tool_name = 'subagent', tool_call_index = 0 } })
  local input = table.concat({ '{"a":1,', '"b":2,', '"c":3,', '"d":4,', '"e":5,', '"f":6,', '"g":7,', '"h":8}' }, '\n')
  apply_render(m, b, { SubAgentInputChunk = { tool_call_index = 0, content = input } })
  apply_render(m, b, { AssistantMessageEnd = {} })
  local sa = m.elements[1]
  apply_render(m, b, { SubAgentStart = { tool_call_id = 'sa1', conversation_id = 'c1', description = 'helper' } })
  check(T.content_of(sa, 'input') == input, 'model keeps the FULL input (no flag truncates it)')
  local l = lines_of(b)
  check(l[4] == '"d":4,' and l[5] == '"e":5,' and l[6] == '"f":6,' and l[7] == '"g":7,' and l[8] == '"h":8}',
    'input shows exactly the last 5 of 8 content rows inside the fence')
  check(l[9] == TC_FENCE and l[10] == '► Output' and l[11] == TC_FENCE and l[12] == '',
    'input close fence + Output header + open fence + empty output row')
end)

test('subagent: post-flush input chunks are absorbed into the input region at SubAgentStart', function()
  -- The settle flush closes the input fence mid-stream; a later chunk must
  -- still accumulate into el.input, and the SubAgentStart rebuild must render
  -- it INSIDE the input fence — no stray or duplicated rows.
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { SubAgentInputStart = { tool_call_id = 'sa1', tool_call_index = 0 } })
  apply_render(m, b, { SubAgentInputChunk = { tool_call_index = 0, content = '{"a":1,\n' } })
  apply_render(m, b, { SubAgentInputChunk = { tool_call_index = 0, content = '"b":2}' } })
  local sa = m.elements[1]
  -- Settle flush closes the input fence (2 visual lines: no collapse).
  render_diff(m, b, T.close_open_elements(m))
  local l = lines_of(b)
  check(sa.input_open == false, 'input fence closed by the flush')
  check(l[1] == '► SUB-AGENT: [generating]' and l[2] == '► Input'
    and l[3] == TC_FENCE and l[4] == '{"a":1,' and l[5] == '"b":2}'
    and l[6] == TC_FENCE and l[7] == '► Output' and l[8] == TC_FENCE and l[9] == '' and l[10] == TC_FENCE,
    'flush rows: label + Input fence pair + Output section')
  -- A chunk arriving after the flush accumulates into el.input.
  apply_render(m, b, { SubAgentInputChunk = { tool_call_index = 0, content = ',"c":3}' } })
  check(T.content_of(sa, 'input') == '{"a":1,\n"b":2},"c":3}', 'post-flush chunk accumulated into the model')
  -- AssistantMessageEnd (as in the real protocol) adds nothing visible.
  apply_render(m, b, { AssistantMessageEnd = {} })
  -- SubAgentStart rebuilds the region from full model state: the post-flush
  -- chunk is absorbed back INSIDE the input fence, no stray/duplicated rows.
  apply_render(m, b, { SubAgentStart = { tool_call_id = 'sa1', conversation_id = 'c1', description = 'helper' } })
  l = lines_of(b)
  check(l[1] == '► SUB-AGENT: [running]  helper' and l[2] == '► Input'
    and l[3] == TC_FENCE and l[4] == '{"a":1,' and l[5] == '"b":2},"c":3}'
    and l[6] == TC_FENCE and l[7] == '► Output' and l[8] == TC_FENCE and l[9] == '',
    'post-flush chunk absorbed into the input region by the rebuild')
  -- The active subagent streams output at its own region tail.
  apply_render(m, b, { AssistantMessageChunk = { content = 'result1' } })
  apply_render(m, b, { AssistantMessageChunk = { content = ' result2' } })
  l = lines_of(b)
  check(l[9] == 'result1 result2', 'output streams at the subagent region tail')
  check(#l == 9, 'no stray rows: exactly the 9-row region')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

-- ------------------------------------------------------------ bug-2 fix

test('bug-2: assistant content streams onto its OWN rows after a tool call', function()
  -- Regression: streamed assistant text used to append at the buffer tail,
  -- landing ON the end_info token line. It must land on the assistant
  -- element's own rows (inside its region, at the arrival position below the
  -- attached thinking block), shifting later elements down.
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { UserMessage = { content = 'turn one' } }) -- rows 0-1
  local um = m.elements[1]
  apply_render(m, b, { AssistantMessageStart = {} })             -- row 2
  local am = m.elements[2]
  apply_render(m, b, { AssistantThinkingChunk = { content = 'think' } })
  local block = m.elements[3]
  apply_render(m, b, { AssistantToolCallStart = { tool_call_id = 't1', tool_name = 'bash', tool_call_index = 0 } })
  local tc = m.elements[4]
  apply_render(m, b, { AssistantToolCallArgChunk = { tool_call_index = 0, content = 'arg' } })
  apply_render(m, b, { ToolMessageStart = { tool_call_id = 't1', tool_args = '' } })
  apply_render(m, b, { ToolOutputChunk = { tool_call_id = 't1', content = 'res' } })
  apply_render(m, b, { ToolMessageEnd = { tool_call_id = 't1', end_status = 'Succeeded', input_tokens = 1, output_tokens = 2 } })
  local info = m.elements[#m.elements]
  -- The assistant message sits ABOVE the tool call; its late-streamed text
  -- must insert inside its own region (below the attached thinking block),
  -- never on the token line.
  apply_render(m, b, { AssistantMessageChunk = { content = ' reply' } })
  local l = lines_of(b)
  -- The thinking block is ATTACHED inside the am's region: the reply lands on
  -- the am's own rows BELOW it (arrival order), never on the buffer tail.
  check(l[4] == '► [Thinking... press o to expand]' and l[5] == ' reply',
    'assistant content on its own rows below the attached thinking hint (never the buffer tail)')
  check(l[#l] == '► [TOOL: 1 in / 2 out tokens]', 'end_info token line intact at the bottom')
  check(l[#l]:find('reply', 1, true) == nil, 'no assistant text after the token line')
  check(row_of(m, am).start_row == 2 and row_of(m, am).height == 3,
    'assistant rows grew in place to label + attached block + reply')
  check(row_of(m, block).start_row == 3 and row_of(m, block).height == 1,
    'thinking block attached above the assistant content (inside the am region)')
  check(row_of(m, tc).start_row == 5, 'tool call shifted down below the assistant content')
  check(row_of(m, info).start_row == 14, 'end_info shifted to the new tail')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

-- ------------------------------------------------------------ invariants

test('modifiable: restored to false after every render call in a long stream', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  local events = {
    { UserMessage = { content = 'turn one' } },
    { AssistantMessageStart = {} },
    { AssistantThinkingChunk = { content = 'think a\nthink b' } },
    { AssistantMessageChunk = { content = ' visible' } },
    { AssistantToolCallStart = { tool_call_id = 't1', tool_name = 'bash', tool_call_index = 0 } },
    { AssistantToolCallArgChunk = { tool_call_index = 0, content = 'x' } },
    { ToolMessageStart = { tool_call_id = 't1', tool_args = '' } },
    { ToolOutputChunk = { tool_call_id = 't1', content = 'res' } },
    { ToolMessageEnd = { tool_call_id = 't1', end_status = 'Succeeded', input_tokens = 1, output_tokens = 2 } },
    { AssistantRequestEnd = { total_input_tokens = 1, total_output_tokens = 2 } },
  }
  local ok = true
  for _, ev in ipairs(events) do
    local p_ok, err = pcall(apply_render, m, b, ev)
    if not p_ok then ok = false end
    if vim.bo[b].modifiable ~= false then ok = false end
  end
  check(ok, 'no error and modifiable false after every render')
end)

test('row map: every element region resolves via element_at_row', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { UserMessage = { content = 'u1\nu2' } }) -- rows 0-2
  local um = m.elements[1]
  apply_render(m, b, { AssistantMessageStart = {} })           -- row 3
  apply_render(m, b, { SubAgentInputStart = { tool_call_id = 'sa1', tool_call_index = 0 } })
  local sa = m.elements[3]
  apply_render(m, b, { AssistantMessageEnd = {} })
  apply_render(m, b, { SubAgentStart = { tool_call_id = 'sa1', conversation_id = 'c1', description = 'd' } })
  apply_render(m, b, { AssistantMessageChunk = { content = 'sub out' } })
  -- user message: label + 2 content rows.
  check(row_of(m, um).start_row == 0 and row_of(m, um).height == 3, 'um covers label + content')
  -- subagent region: label + Output header + fence + streamed output row
  -- (the output fence stays open while the subagent streams).
  local r = row_of(m, sa)
  check(r.start_row == 4 and r.height == 4, 'subagent region grew with the output')
  local el, off = T.element_at_row_full(m, b, 4)
  check(el == sa and off == 0, 'subagent label row resolves to (sa, 0)')
  el, off = T.element_at_row_full(m, b, 7)
  check(el == sa and off == 3, 'subagent last row resolves to (sa, 3)')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('hand-built diff: a direct updated_content entry applies without apply()', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  local d = T.apply(m, { AssistantMessageStart = {} })
  T.render(m, d, { buf = b, ns = ns, bulk = false })
  local am = m.elements[1]
  -- Hand-build the next diff exactly as the reducer would emit it.
  T.render(m, { added = {}, updated_all = {}, updated_content = { { am, 'hello' } } },
    { buf = b, ns = ns, bulk = false })
  local l = lines_of(b)
  check(l[1] == '► ASSISTANT' and l[2] == 'hello', 'hand-built delta inserts after the label')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('element_at_row: resolves user/tool/subagent regions, nil beyond the buffer', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { UserMessage = { content = 'u1\nu2' } }) -- rows 0-2
  local um = m.elements[1]
  apply_render(m, b, { AssistantToolCallStart = { tool_call_id = 't', tool_call_index = 0 } })
  local tc = m.elements[2]
  apply_render(m, b, { AssistantToolCallArgChunk = { tool_call_index = 0, content = 'arg' } })
  apply_render(m, b, { ToolMessageStart = { tool_call_id = 't', tool_args = '' } })
  apply_render(m, b, { SubAgentInputStart = { tool_call_id = 's', tool_call_index = 1 } })
  apply_render(m, b, { AssistantMessageEnd = {} })
  apply_render(m, b, { SubAgentStart = { tool_call_id = 's', conversation_id = 'c', description = 'd' } })
  local sa = m.elements[3]
  check(T.element_at_row(m, b, 0) == um, 'user message at its label row')
  check(T.element_at_row(m, b, 2) == um, 'user message at its last content row')
  local el, off = T.element_at_row_full(m, b, row_of(m, tc).start_row)
  check(el == tc and off == 0, 'tool call at its label row (offset 0)')
  el, off = T.element_at_row_full(m, b, row_of(m, sa).start_row)
  check(el == sa and off == 0, 'subagent at its label row (offset 0)')
  check(T.element_at_row(m, b, vim.api.nvim_buf_line_count(b)) == nil, 'nil beyond the last row')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('gb: element_at_row on a user message resolves the envelope msg_id', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  local d = T.apply(m, { UserMessage = { content = 'branch me\nsecond line' } }, 42)
  T.render(m, d, { buf = b, ns = ns, bulk = false })
  local el = T.element_at_row(m, b, 0)
  check(el == m.elements[1] and el.type == 'user_message', 'user message resolved at its label row')
  check(el.msg_id == 42, 'msg_id matches the envelope id passed to apply')
  check(T.element_at_row(m, b, 1) == el, 'content rows resolve to the same element')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

-- -------------------------------------------------------- toggles + action_at

test('o dispatch: a collapsed thinking block is ONE navigable real row', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantMessageStart = {} })
  apply_render(m, b, { AssistantThinkingChunk = { content = 'A\nB\nC' } })
  local block = m.elements[2]
  render_diff(m, b, T.close_open_elements(m))
  local l = lines_of(b)
  check(#l == 2, 'collapsed block is one real row below the label')
  local el, off = T.element_at_row_full(m, b, 1)
  check(el == block and off == 0, 'the collapsed row resolves to the block')
  check(T.action_at(block, 0) == 'thinking', '`o` on the collapsed row toggles thinking')
  -- Expand via the keymap-equivalent reducer + render.
  render_diff(m, b, T.toggle_thinking_element(m, block))
  l = lines_of(b)
  check(l[2] == '► [Thinking... press o to collapse]' and l[3] == 'A' and l[4] == 'B' and l[5] == 'C',
    'expanded content restored')
  check(T.action_at(block, 2) == 'thinking', '`o` on expanded content toggles too')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('o dispatch: every tool row resolves to the detail intent', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantMessageStart = {} })
  apply_render(m, b, { AssistantToolCallStart = { tool_name = 'write', tool_call_id = 't1', tool_call_index = 0 } })
  apply_render(m, b, { AssistantToolCallArgChunk = { tool_call_index = 0, content = 'a\nb\nc\nd' } })
  local tc = m.elements[2]
  apply_render(m, b, { ToolMessageStart = { tool_call_id = 't1', tool_name = 'write' } })
  local l = lines_of(b)
  -- label row 1; Param header row 2; first arg row 4; Result header row 9.
  check(l[2] == '► TOOL: [running] write  [Ctrl-k to cancel]', 'label row')
  check(l[3] == '► Param', 'Param header row')
  check(l[5] == 'a' and l[6] == 'b' and l[7] == 'c' and l[8] == 'd', 'full args rows')
  local el, off = T.element_at_row_full(m, b, 4)
  check(el == tc and off == 3 and T.action_at(tc, 3) == 'detail', 'args row -> (tc, 3) detail')
  el, off = T.element_at_row_full(m, b, 1)
  check(el == tc and off == 0 and T.action_at(tc, 0) == 'detail', 'label row -> (tc, 0) detail')
  el, off = T.element_at_row_full(m, b, 9)
  check(el == tc and off == 8 and T.action_at(tc, 8) == 'detail', 'Result header row -> (tc, 8) detail')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('label text: status changes rebuild the real label row, no stacking', function()
  -- Regression: labels used to be virt-text overlays that stacked on status
  -- changes. Now the label is REAL buffer text, rebuilt per status change.
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { SubAgentInputStart = { tool_call_id = 'sa1', tool_call_index = 0 } })
  apply_render(m, b, { AssistantMessageEnd = {} })
  apply_render(m, b, { SubAgentStart = { tool_call_id = 'sa1', conversation_id = 'c1', description = 'helper' } })
  for _ = 1, 5 do
    apply_render(m, b, { SubAgentWaitingPermission = { conversation_id = 'c1' } })
    apply_render(m, b, { SubAgentPermissionApproved = { conversation_id = 'c1' } })
  end
  apply_render(m, b, { SubAgentEnd = { conversation_id = 'c1', end_status = 'Succeeded', input_tokens = 1, output_tokens = 2 } })
  local l = lines_of(b)
  check(l[1] == '► SUB-AGENT: [done]  [1 in / 2 out]  helper', 'label rebuilt as real text')
  local label_rows = 0
  for _, line in ipairs(l) do
    if line:find('SUB%-AGENT', 1) then label_rows = label_rows + 1 end
  end
  check(label_rows == 1, 'exactly one subagent label row after many status changes')
  check(row_of(m, m.elements[1]).start_row == 0, 'subagent still starts at row 0')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

-- ------------------------------------------------ highlight-mark bounds

test('hl bound: thinking block keeps one mark per row across collapse/expand cycles', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantMessageStart = {} })
  apply_render(m, b, { AssistantThinkingChunk = { content = 'L1\nL2\nL3' } })
  local block = m.elements[2]
  check(count_hl(b, 'TCodeThinking') == 3, 'streamed content highlighted once per row')
  render_diff(m, b, T.close_open_elements(m))
  check(count_hl(b, 'TCodeThinking') == 0, 'collapse removes every content highlight')
  for i = 1, 5 do
    render_diff(m, b, T.toggle_thinking_element(m, block)) -- collapsed -> expanded
    check(count_hl(b, 'TCodeThinking') == 3, 'expand places one mark per content row')
    render_diff(m, b, T.toggle_thinking_element(m, block)) -- expanded -> collapsed
    check(count_hl(b, 'TCodeThinking') == 0, 'collapse removes every content highlight')
  end
  -- Merge reopen after a collapse: the full content renders fresh, then
  -- streaming must not stack duplicate marks on the join row.
  apply_render(m, b, { AssistantThinkingChunk = { content = 'M1' } }) -- merge reopen
  check(count_hl(b, 'TCodeThinking') == 3, 'merge-reopened content highlighted once per row')
  for _ = 1, 50 do
    apply_render(m, b, { AssistantThinkingChunk = { content = 'x' } })
  end
  check(count_hl(b, 'TCodeThinking') == 3, '50 newline-less chunks after the reopen add no marks')
  check(lines_of(b)[4] == 'L3M1' .. string.rep('x', 50), 'chunks joined the reopened tail row')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('hl dedup: newline-less chunks stack no marks on the thinking join row', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantMessageStart = {} })
  apply_render(m, b, { AssistantThinkingChunk = { content = 'A\nB' } })
  local block = m.elements[2]
  check(count_hl(b, 'TCodeThinking') == 2, 'two content rows highlighted')
  -- 50 single-char chunks all join the 'B' row: distinct content rows ==
  -- highlight marks, the join row keeps exactly one.
  for _ = 1, 50 do
    apply_render(m, b, { AssistantThinkingChunk = { content = 'x' } })
  end
  check(count_hl(b, 'TCodeThinking') == 2, '50 newline-less chunks add no marks')
  local join_row_marks = 0
  local marks = vim.api.nvim_buf_get_extmarks(b, ns, 0, -1, { details = true })
  for _, mm in ipairs(marks) do
    if mm[4] and mm[4].hl_group == 'TCodeThinking' and mm[2] == 2 then join_row_marks = join_row_marks + 1 end
  end
  check(join_row_marks == 1, 'join row carries exactly one highlight mark')
  check(lines_of(b)[2] == 'A' and lines_of(b)[3] == 'B' .. string.rep('x', 50), 'chunks joined the B row')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('hl bound: tool args highlights stay bounded across permission cycles', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantToolCallStart = { tool_call_id = 't1', tool_name = 'bash', tool_call_index = 0 } })
  apply_render(m, b, { AssistantToolCallArgChunk = { tool_call_index = 0, content = 'a\nb\nc\nd' } })
  local tc = m.elements[1]
  check(count_hl(b, 'TCodeToolArgs') == 4, 'four streamed args rows highlighted')
  apply_render(m, b, { ToolMessageStart = { tool_call_id = 't1', tool_args = '' } })
  -- Content rows: the 4 args rows + the empty output row; the chrome rows
  -- (label / headers) carry their own col-range marks, never TCodeToolArgs.
  check(count_hl(b, 'TCodeToolArgs') == 5, 'four args rows + the empty output row highlighted')
  local marks = vim.api.nvim_buf_get_extmarks(b, ns, 0, -1, { details = true })
  local on_first_arg = false
  for _, mm in ipairs(marks) do
    if mm[4] and mm[4].hl_group == 'TCodeToolArgs' and mm[2] == 3 then on_first_arg = true end
  end
  check(on_first_arg, 'first args row 3 highlighted TCodeToolArgs')
  -- Permission cycles rebuild the region: the count must stay 5, not grow.
  for _ = 1, 10 do
    apply_render(m, b, { ToolRequestPermission = { tool_call_id = 't1' } })
    check(count_hl(b, 'TCodeToolArgs') == 5, 'args highlight count stable at permission')
    apply_render(m, b, { ToolPermissionApproved = { tool_call_id = 't1' } })
    check(count_hl(b, 'TCodeToolArgs') == 5, 'args highlight count stable after approval')
  end
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('hl dedup: newline-less chunks stack no marks on the tool args join row', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantToolCallStart = { tool_call_id = 't1', tool_name = 'bash', tool_call_index = 0 } })
  apply_render(m, b, { AssistantToolCallArgChunk = { tool_call_index = 0, content = 'a\nb' } })
  local tc = m.elements[1]
  check(count_hl(b, 'TCodeToolArgs') == 2, 'two args rows highlighted')
  -- 50 single-char chunks join the 'b' row: distinct content rows == marks.
  for _ = 1, 50 do
    apply_render(m, b, { AssistantToolCallArgChunk = { tool_call_index = 0, content = 'x' } })
  end
  check(count_hl(b, 'TCodeToolArgs') == 2, '50 newline-less chunks add no marks')
  local join_row_marks = 0
  local marks = vim.api.nvim_buf_get_extmarks(b, ns, 0, -1, { details = true })
  for _, mm in ipairs(marks) do
    if mm[4] and mm[4].hl_group == 'TCodeToolArgs' and mm[2] == 4 then join_row_marks = join_row_marks + 1 end
  end
  check(join_row_marks == 1, 'join row carries exactly one highlight mark')
  check(lines_of(b)[4] == 'a' and lines_of(b)[5] == 'b' .. string.rep('x', 50), 'chunks joined the b row')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

-- ------------------------------------------------------------- render_batch

test('render_batch: applies multiple diffs in order and restores modifiable', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  local d0 = T.apply(m, { AssistantMessageStart = {} })
  T.render(m, d0, { buf = b, ns = ns, bulk = false })
  local d1 = T.apply(m, { AssistantMessageChunk = { content = 'one' } })
  local d2 = T.apply(m, { AssistantMessageChunk = { content = ' two' } })
  local d3 = T.apply(m, { AssistantMessageChunk = { content = ' three' } })
  T.render_batch(m, { d1, d2, d3 }, { buf = b, ns = ns, bulk = false })
  local l = lines_of(b)
  check(l[1] == '► ASSISTANT' and l[2] == 'one two three', 'three content deltas applied in order')
  check(vim.bo[b].modifiable == false, 'modifiable restored to false after the batch')
end)

test('render_batch: cursor follows the stream when at the bottom (headless window)', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  local d0 = T.apply(m, { AssistantMessageStart = {} })
  T.render(m, d0, { buf = b, ns = ns, bulk = false })
  -- A real floating window on the buffer so bufwinid resolves and the cursor
  -- position is trackable in headless mode (verified stable in this harness).
  local win = vim.api.nvim_open_win(b, false, { relative = 'editor', width = 40, height = 20, row = 1, col = 1 })
  vim.api.nvim_set_current_win(win)
  local set_ok, set_err = pcall(vim.api.nvim_win_set_cursor, win, { vim.api.nvim_buf_line_count(b), 0 })
  check(set_ok, 'cursor parked at the bottom: ' .. tostring(set_err))
  -- Append below the parked cursor; render_batch must move the cursor to the
  -- new bottom (was_at_bottom computed before the writes).
  local d1 = T.apply(m, { AssistantMessageChunk = { content = 'follow me' } })
  local d2 = T.apply(m, { AssistantMessageChunk = { content = '\nline two' } })
  local r_ok, r_err = pcall(T.render_batch, m, { d1, d2 }, { buf = b, ns = ns, bulk = false })
  check(r_ok, 'render_batch ran without error: ' .. tostring(r_err))
  local cursor = vim.api.nvim_win_get_cursor(win)
  local line_count = vim.api.nvim_buf_line_count(b)
  check(cursor[1] == line_count, 'cursor followed to the new bottom line')
  local l = lines_of(b)
  check(l[#l] == 'line two', 'streamed content visible at the new bottom')
  vim.api.nvim_win_close(win, true)
  check(vim.bo[b].modifiable == false, 'modifiable restored to false after the batch')
end)

-- --------------------------------------------- output auto-collapse

test('tool output: a long result caps at 5 tail lines; the detail intent is uniform', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantMessageStart = {} })
  apply_render(m, b, { AssistantToolCallStart = { tool_name = 'read', tool_call_id = 't1', tool_call_index = 0 } })
  apply_render(m, b, { ToolMessageStart = { tool_call_id = 't1', tool_name = 'read' } })
  local tc = m.elements[2]
  apply_render(m, b, { ToolOutputChunk = { tool_call_id = 't1', content = 'o1\n' } })
  apply_render(m, b, { ToolOutputChunk = { tool_call_id = 't1', content = 'o2\no3\no4' } })
  check(T.content_of(tc, 'output') == 'o1\no2\no3\no4', 'output accumulated while streaming')

  -- ToolMessageEnd closes the fence; the 4 output lines fit the tail cap.
  apply_render(m, b, { ToolMessageEnd = { tool_call_id = 't1', end_status = 'Succeeded', input_tokens = 1, output_tokens = 4 } })
  local l = lines_of(b)
  check(l[1] == '► ASSISTANT', 'assistant label intact')
  check(l[2] == '► TOOL: [done] read' and l[3] == '► Result' and l[4] == TC_FENCE,
    'tool label + Result header + open fence')
  check(l[5] == 'o1' and l[6] == 'o2' and l[7] == 'o3' and l[8] == 'o4' and l[9] == TC_FENCE,
    'output rows inside the closed fence pair')
  check(l[10] == '► [TOOL: 1 in / 4 out tokens]', 'end_info below the tool region')

  -- Every tool row (label, headers, fences, content) resolves to detail.
  for row = 1, 8 do
    local el, off = T.element_at_row_full(m, b, row)
    check(el == tc and T.action_at(tc, off) == 'detail', ('tool row %d -> detail'):format(row))
  end
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('subagent output: the output fence closes at SubAgentEnd; the detail intent is uniform', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { SubAgentInputStart = { tool_call_id = 'sa1', tool_call_index = 0 } })
  apply_render(m, b, { AssistantMessageEnd = {} })
  apply_render(m, b, { SubAgentStart = { tool_call_id = 'sa1', conversation_id = 'c1', description = 'helper' } })
  local sa = m.elements[1]
  apply_render(m, b, { AssistantMessageChunk = { content = 's1\n' } })
  apply_render(m, b, { AssistantMessageChunk = { content = 's2\ns3\ns4' } })
  check(T.content_of(sa, 'output') == 's1\ns2\ns3\ns4', 'output accumulated while streaming')
  -- While the subagent streams, the Output fence stays OPEN (no close fence).
  local l = lines_of(b)
  check(l[#l] == 's4', 'no close fence while the subagent streams')
  apply_render(m, b, { SubAgentEnd = { conversation_id = 'c1', end_status = 'Succeeded', input_tokens = 1, output_tokens = 4 } })
  l = lines_of(b)
  check(l[#l] == TC_FENCE, 'output close fence appears only after SubAgentEnd')
  check(l[4] == 's1' and l[5] == 's2' and l[6] == 's3' and l[7] == 's4',
    'output rows inside the closed fence pair')
  local el, off = T.element_at_row_full(m, b, 5)
  check(el == sa and T.action_at(sa, off) == 'detail', 'subagent output row resolves to detail')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

-- --------------------------------------------- streaming tail cap

test('streaming tail: a 10-line tool output stays capped at 5 rows while streaming and after close', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  windowed_render(b, { AssistantToolCallStart = { tool_call_id = 't1', tool_name = 'bash', tool_call_index = 0 } }, false)
  windowed_render(b, { ToolMessageStart = { tool_call_id = 't1', tool_args = '' } }, false)
  local tc = m.elements[1]
  for i = 1, 10 do
    windowed_render(b, { ToolOutputChunk = { tool_call_id = 't1', content = 'o' .. i .. '\n' } }, false)
  end
  -- The output has 10 lines; the tail cap keeps exactly the last 5.
  local l = lines_of(b)
  check(l[4] == 'o6' and l[5] == 'o7' and l[6] == 'o8' and l[7] == 'o9' and l[8] == 'o10',
    'exactly 5 content rows while streaming (the last 5 of 10)')
  check(#l == 8, 'streaming region: label + Result + fence + 5 content (no close fence)')
  check(row_of(m, tc).height == 8, 'row map height matches the 8 streaming rows')
  -- Every streaming row resolves to the tool with a detail intent.
  for row = 0, 7 do
    local el, off = T.element_at_row_full(m, b, row)
    check(el == tc and off == row and T.action_at(tc, off) == 'detail',
      ('streaming row %d -> (tc, %d) detail'):format(row, row))
  end
  -- ToolMessageEnd closes the section: same 5 content rows + close fence.
  windowed_render(b, { ToolMessageEnd = { tool_call_id = 't1', end_status = 'Succeeded', input_tokens = 1, output_tokens = 10 } }, false)
  l = lines_of(b)
  check(l[4] == 'o6' and l[5] == 'o7' and l[6] == 'o8' and l[7] == 'o9' and l[8] == 'o10' and l[9] == TC_FENCE,
    'closed region: same 5 content rows + close fence')
  check(row_of(m, tc).height == 9, 'row map height grows by the close fence')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('header placement: Param and Result headers sit outside the fence pairs', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantToolCallStart = { tool_call_id = 't1', tool_name = 'bash', tool_call_index = 0 } })
  apply_render(m, b, { AssistantToolCallArgChunk = { tool_call_index = 0, content = '{"a":1}' } })
  apply_render(m, b, { ToolMessageStart = { tool_call_id = 't1', tool_args = '' } })
  apply_render(m, b, { ToolOutputChunk = { tool_call_id = 't1', content = 'res' } })
  local l = lines_of(b)
  -- label, Param, fence, args, fence, Result, fence, output (streaming: no close)
  check(l[1]:find('TOOL:', 1, true) ~= nil, 'label row first')
  check(l[2] == '► Param' and l[3] == TC_FENCE and l[4] == '{"a":1}' and l[5] == TC_FENCE,
    'Param header outside the args fence pair')
  check(l[6] == '► Result' and l[7] == TC_FENCE and l[8] == 'res',
    'Result header outside the output fence (open while streaming)')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('label colors: chrome extmarks carry per-part groups on the label row', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantToolCallStart = { tool_call_id = 't1', tool_name = 'bash',
    tool_call_index = 0, created_at = 1700000000000 } })
  local l = lines_of(b)
  check(l[1] == '► TOOL: [generating] bash  17:13:20  [Ctrl-k to cancel]', 'label text with timestamp')
  local marks = vim.api.nvim_buf_get_extmarks(b, ns, 0, -1, { details = true })
  local function col_mark(group, col)
    for _, mm in ipairs(marks) do
      if mm[4] and mm[4].hl_group == group and mm[2] == 0 and mm[3] == col then
        return mm[4]
      end
    end
    return nil
  end
  -- '► TOOL:' [0,9) TCodeTool; ' [generating]' [9,22) TCodeTool; ' bash'
  -- [22,27) TCodeTool; '  17:13:20' [27,37) TCodeTokens; '  [Ctrl-k to
  -- cancel]' [37,57) TCodeTokens.
  local d = col_mark('TCodeTool', 0)
  check(d ~= nil and d.end_col == 9, 'TCodeTool over ► TOOL:')
  d = col_mark('TCodeTool', 9)
  check(d ~= nil and d.end_col == 22, 'TCodeTool over the status')
  d = col_mark('TCodeTool', 22)
  check(d ~= nil and d.end_col == 27, 'TCodeTool over the tool name')
  d = col_mark('TCodeTokens', 27)
  check(d ~= nil and d.end_col == 37, 'TCodeTokens over the timestamp')
  d = col_mark('TCodeTokens', 37)
  check(d ~= nil and d.end_col == 57, 'TCodeTokens over the cancel hint')
  -- The chrome marks beat the treesitter default priority.
  d = col_mark('TCodeTool', 0)
  check(d ~= nil and (d.priority or 0) >= 150, 'chrome mark priority is at least 150')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

-- ------------------------------------------------- multi-line content safety

test('retry: a multi-line reason renders split rows, no embedded newlines', function()
  -- Regression: an LLMRetry reason can be a multi-line JSON error body; it
  -- must not reach nvim_buf_set_lines as a single string containing '\n'
  -- ('replacement string item contains newlines').
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  local reason = 'Failed to get access token: {\n  "error": {\n    "message": "refresh token reused"\n  }\n}'
  local ok, err = pcall(apply_render, m, b, { LLMRetry = { attempt = 1, max_retries = 3, reason = reason } })
  check(ok, 'LLMRetry renders without error: ' .. tostring(err))
  local l = lines_of(b)
  check(l[1] == '► [Retrying... (attempt 1/3) -- Failed to get access token: {]', 'header row with the first reason line')
  check(l[2] == '  "error": {' and l[3] == '    "message": "refresh token reused"', 'reason split across rows')
  check(l[4] == '  }' and l[5] == '}', 'closing JSON rows')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('session: OpenAI auth-failure turn (retries + multi-line end error) renders cleanly', function()
  -- The exact event sequence that crashed a fresh OpenAI session.
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  local reason = 'Failed to get access token: OpenAI token refresh failed (401 Unauthorized): {\n  \"error\": {\n    \"message\": \"reused\"\n  }\n}'
  local events = {
    { UserMessage = { content = 'hi', media_filenames = {} } },
    { AssistantMessageStart = {} },
    { LLMRetry = { attempt = 1, max_retries = 3, reason = reason } },
    { LLMRetry = { attempt = 2, max_retries = 3, reason = reason } },
    { LLMRetry = { attempt = 3, max_retries = 3, reason = reason } },
    { AssistantMessageEnd = { end_status = 'Failed', error = reason, input_tokens = 0, output_tokens = 0 } },
  }
  local ok, err
  for _, ev in ipairs(events) do
    ok, err = pcall(apply_render, m, b, ev)
    if not ok then break end
  end
  check(ok, 'full turn renders without error: ' .. tostring(err))
  local l = lines_of(b)
  check(l[1] == '► USER' and l[2] == 'hi', 'user message rendered')
  check(l[3] == '► ASSISTANT', 'assistant label rendered (no trailing blank)')
  -- Each retry block is a 5-row header + reason body (5 reason lines each).
  check(l[4]:find('Retrying%.%.%. %(attempt 1/3%)', 1) ~= nil, 'first retry header')
  check(l[9]:find('Retrying%.%.%. %(attempt 2/3%)', 1) ~= nil, 'second retry header')
  check(l[14]:find('Retrying%.%.%. %(attempt 3/3%)', 1) ~= nil, 'third retry header')
  check(l[19] == '► [0 in / 0 out tokens] [Failed]', 'end info token + status row')
  check(l[20] == 'Error: Failed to get access token: OpenAI token refresh failed (401 Unauthorized): {', 'end error first line')
  check(l[24] == '}', 'end error last line')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('confirm_popup prompt: embedded newlines are collapsed to a single line', function()
  -- Regression: the <C-k> cancel prompts interpolate wire-derived descriptions
  -- / tool names that can contain '\n'. confirm_popup writes the prompt as ONE
  -- buffer line via nvim_buf_set_lines, which rejects a string containing an
  -- embedded newline (crash inside the keymap callback). The prompt must be
  -- sanitized with gsub('\n', ' ') before reaching the popup buffer.
  local b = new_buf()
  local raw = "Cancel subagent 'multi\nline desc'? (y/n)"
  -- Demonstrate the crash mechanism: a multi-line item is rejected.
  local raw_ok = pcall(function()
    T.with_modifiable(b, function()
      vim.api.nvim_buf_set_lines(b, 0, -1, false, { raw })
    end)
  end)
  check(not raw_ok, 'nvim_buf_set_lines rejects an embedded newline (the crash)')
  -- The sanitized form (what the keymap now builds) writes cleanly.
  local sanitized = raw:gsub('\n', ' ')
  check(sanitized:find('\n', 1, true) == nil, 'sanitization removes embedded newlines')
  local ok, err = pcall(function()
    T.with_modifiable(b, function()
      vim.api.nvim_buf_set_lines(b, 0, -1, false, { sanitized })
    end)
  end)
  check(ok, 'sanitized single-line prompt accepted: ' .. tostring(err))
  check(lines_of(b)[1] == "Cancel subagent 'multi line desc'? (y/n)", 'prompt text preserved (newline -> space)')
  -- Tool-name flavor uses the same sanitization.
  local tool_prompt = ("Cancel tool '%s'? (y/n)"):format(('read\nscript'):gsub('\n', ' '))
  check(tool_prompt == "Cancel tool 'read script'? (y/n)", 'tool prompt sanitized')
  check(tool_prompt:find('\n', 1, true) == nil, 'tool prompt single-line')
end)

test('labels: multi-line tool_name / subagent description are flattened into real text', function()
  -- Wire-derived label text is now REAL buffer text, which must stay on one
  -- row; a '\n' in the tool_name or description must be collapsed.
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  local ok, err = pcall(apply_render, m, b,
    { AssistantToolCallStart = { tool_name = 'read\nscript', tool_call_id = 't1', tool_call_index = 0 } })
  check(ok, 'multi-line tool_name renders without error: ' .. tostring(err))
  local l = lines_of(b)
  check(l[1] == '► TOOL: [generating] read script  [Ctrl-k to cancel]' and #l == 1,
    'label-only tool region, name flattened')

  local m2 = T.reset_model()
  local b2 = new_buf()
  seed(b2, { '' })
  apply_render(m2, b2, { SubAgentInputStart = { tool_call_id = 's', tool_call_index = 0 } })
  local ok2, err2 = pcall(apply_render, m2, b2,
    { SubAgentStart = { tool_call_id = 's', conversation_id = 'c1', description = 'multi\nline' } })
  check(ok2, 'multi-line subagent description renders without error: ' .. tostring(err2))
  local l2 = lines_of(b2)
  check(l2[1] == '► SUB-AGENT: [running]  multi line' and l2[2] == '► Output' and l2[3] == TC_FENCE,
    'subagent region rows intact')
  check(vim.bo[b].modifiable == false and vim.bo[b2].modifiable == false, 'buffers non-modifiable')
end)
