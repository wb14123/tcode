-- Buffer-level tests for the RENDERER layer of tcode.lua. Every test drives
-- `T.apply(model, event)` then `T.render(model, diff, ctx)` with a real scratch
-- buffer, and asserts the projected buffer rows, extmarks, and the modifiable
-- invariant. The renderer is the ONLY layer that writes the buffer; the diff
-- contract comes from the reducer (added / updated_all / updated_content).

local TC_FENCE = string.rep('`', 10)
local tc_ns_id = vim.api.nvim_get_namespaces()['tcode_tc_id']
local sa_ns_id = vim.api.nvim_get_namespaces()['tcode_sa_id']
local um_ns_id = vim.api.nvim_get_namespaces()['tcode_um']
local gen_ns_id = vim.api.nvim_get_namespaces()['tcode_gen']

-- Apply one event to the model and render the resulting diff into the buffer
-- (mirrors how the reader will call it: ONE event per render call).
local function apply_render(m, b, event, bulk)
  local d = T.apply(m, event)
  T.render(m, d, { buf = b, ns = ns, bulk = bulk or false })
end

-- Render a reducer-level operation's diff (collapse/toggle) into the buffer.
local function render_diff(m, b, d)
  T.render(m, d, { buf = b, ns = ns, bulk = false })
end

-- The thinking indicator / expand-hint mark for an element in thinking_ns.
local function thinking_mark_for(b, el_id)
  local marks = vim.api.nvim_buf_get_extmarks(b, thinking_ns_id, 0, -1, { details = true })
  for _, m in ipairs(marks) do
    if m[4] and m[4].virt_text then return m end
  end
  return nil
end

-- Find a thinking_ns mark by its id (ids are preserved across toggles).
local function mark_by_id(b, mark_id)
  local marks = vim.api.nvim_buf_get_extmarks(b, thinking_ns_id, 0, -1, { details = true })
  for _, m in ipairs(marks) do
    if m[1] == mark_id then return m end
  end
  return nil
end

-- ------------------------------------------------------------------ basics

test('first_event: the first added replaces the initial empty row', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantMessageStart = {} })
  local l = lines_of(b)
  check(l[1] == '► ASSISTANT' and l[2] == '', 'label + trailing blank replace row 0')
  check(#l == 2, 'no leftover initial empty row')
  -- Subsequent adds append at the tail (the first chunk consumes the blank).
  apply_render(m, b, { AssistantMessageChunk = { content = 'hi' } })
  l = lines_of(b)
  check(l[2] == 'hi', 'first chunk streams onto the trailing blank')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('added: user_message renders label + content with a um nav range', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { UserMessage = { content = 'hello\nworld', created_at = 1000 } })
  local l = lines_of(b)
  check(l[1] == '► USER' and l[2] == 'hello' and l[3] == 'world', 'user rows: label + content')
  local marks = vim.api.nvim_buf_get_extmarks(b, um_ns_id, 0, -1, { details = true })
  check(#marks == 1 and marks[1][2] == 0 and marks[1][4].end_row == 3, 'um nav [0,3) exclusive')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('added: assistant_message renders label + trailing blank', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantMessageStart = { created_at = 5 } })
  local l = lines_of(b)
  check(l[1] == '► ASSISTANT' and l[2] == '', 'label + blank rows')
  check(#l == 2, 'exactly two rows')
end)

test('added: system_message rows are highlighted per level', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { SystemMessage = { level = 'Warning', message = 'disk full' } })
  local l = lines_of(b)
  check(l[1] == '► SYSTEM' and l[2] == 'disk full', 'system rows')
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
  check(l[1] == '[Retrying... (attempt 2/3) -- timeout]', 'retry row text')
  local marks = vim.api.nvim_buf_get_extmarks(b, ns, 0, -1, { details = true })
  local hl = false
  for _, mm in ipairs(marks) do
    if mm[4] and mm[4].hl_group == 'TCodeTokens' and mm[2] == 0 then hl = true end
  end
  check(hl, 'retry row highlighted TCodeTokens')
end)

test('added: media renders blank + markdown image link from the session dir', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  M.display_file = tmp_dir .. '/session/display.jsonl'
  apply_render(m, b, { AssistantMediaOutput = { media = { relative_path = 'uuid.png' } } })
  M.display_file = nil -- restore immediately; later tests must not see it
  local l = lines_of(b)
  local expected = '![img](file://'
    .. vim.uri_encode(vim.fn.fnamemodify(tmp_dir .. '/session/display.jsonl', ':h') .. '/media/uuid.png')
    .. ')'
  check(l[1] == '' and l[2] == expected, 'blank + encoded image link rows')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('added: media without M.display_file is skipped entirely', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  M.display_file = nil
  apply_render(m, b, { AssistantMessageStart = {} })
  apply_render(m, b, { AssistantMediaOutput = { media = { relative_path = 'x.png' } } })
  local l = lines_of(b)
  check(#l == 2 and l[1] == '► ASSISTANT', 'media adds nothing without display_file')
end)

test('added: end_info renders the INFO row, token overlay, and error rows', function()
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
  check(l[1] == '► ASSISTANT' and l[2] == '' and l[3] == '► INFO' and l[4] == 'Error: boom',
    "rows: ['► ASSISTANT','','► INFO','Error: boom']")
  local marks = vim.api.nvim_buf_get_extmarks(b, ns, 0, -1, { details = true })
  local overlay_ok, error_hl = false, false
  for _, mm in ipairs(marks) do
    if mm[4] and mm[4].virt_text and mm[2] == 2 then
      local joined = ''
      for _, part in ipairs(mm[4].virt_text) do joined = joined .. part[1] end
      if joined:find('[1 in / 4 cache read / 2 out tokens]', 1, true) and joined:find('[Failed]', 1, true) then
        overlay_ok = true
      end
    end
    if mm[4] and mm[4].hl_group == 'TCodeError' and mm[2] == 3 then error_hl = true end
  end
  check(overlay_ok, 'token + status overlay on the INFO row')
  check(error_hl, 'error row highlighted TCodeError')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('added: end_info with nothing to show is skipped', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantMessageStart = {} })
  apply_render(m, b, { AssistantMessageEnd = {} })
  local l = lines_of(b)
  check(#l == 2 and l[1] == '► ASSISTANT', 'empty end_info writes no rows')
end)

test('added: end_marker renders ► END with the token-total overlay', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantRequestEnd = {
    total_input_tokens = 10, total_output_tokens = 20,
    total_cache_creation_tokens = 30, total_cache_read_tokens = 40,
  } })
  local l = lines_of(b)
  check(l[1] == '► END', "'► END' row")
  local marks = vim.api.nvim_buf_get_extmarks(b, ns, 0, -1, { details = true })
  local overlay_ok = false
  for _, mm in ipairs(marks) do
    if mm[4] and mm[4].virt_text and mm[2] == 0 then
      local joined = ''
      for _, part in ipairs(mm[4].virt_text) do joined = joined .. part[1] end
      if joined == '[Total: 40 in / 40 cache read / 20 out tokens]' then overlay_ok = true end
    end
  end
  check(overlay_ok, 'total token overlay on the END row')
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
  check(block.anchor ~= nil, 'start anchor extmark placed')

  -- Collapse: indicator + 2 spacer rows, mark at the anchor row.
  render_diff(m, b, T.close_open_elements(m))
  l = lines_of(b)
  check(#l == 4 and l[2] == '' and l[3] == '' and l[4] == '', 'collapsed to indicator + spacers')
  local mark = thinking_mark_for(b, block.id)
  check(mark ~= nil and mark[2] == 1, 'indicator mark at the anchor row')
  check(mark and mark[4].virt_text[1][1] == '[Thinking... press o to expand]', 'indicator text')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')

  -- Expand: full content + collapse-hint range mark.
  render_diff(m, b, T.toggle_thinking_element(m, block))
  l = lines_of(b)
  check(l[2] == 'A1' and l[3] == 'A2', 'expanded content restored')
  local marks = vim.api.nvim_buf_get_extmarks(b, thinking_ns_id, 0, -1, { details = true })
  local expanded = false
  for _, mm in ipairs(marks) do
    if mm[4] and mm[4].end_row == 3 and mm[4].virt_lines then expanded = true end
  end
  check(expanded, 'expanded range mark with end_row 3 and virt_lines hint')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')

  -- Collapse again: back to 3 rows + indicator.
  render_diff(m, b, T.toggle_thinking_element(m, block))
  l = lines_of(b)
  check(l[2] == '' and l[3] == '' and l[4] == '', 'collapsed again')
  mark = thinking_mark_for(b, block.id)
  check(mark ~= nil and mark[2] == 1, 'indicator mark restored')
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
  check(before[2] == '' and before[3] == '' and before[4] == '', 'collapsed indicator + spacers')
  -- An empty thinking chunk (e.g. a burst boundary) must not reopen the block
  -- or replace the indicator rows with a single blank row.
  apply_render(m, b, { AssistantThinkingChunk = { content = '' } })
  local after = lines_of(b)
  check(table.concat(after, '|') == table.concat(before, '|'), 'buffer unchanged by the empty chunk')
  check(block.state == 'collapsed', 'block stays collapsed')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('merge: runs split by a pause render only the new chunk, model holds all', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantMessageStart = {} })
  apply_render(m, b, { AssistantThinkingChunk = { content = 'A1\nA2' } })
  local block = m.elements[2]
  render_diff(m, b, T.close_open_elements(m))
  -- Run 2 arrives after the pause: merge reopen must NOT re-render the old
  -- content (it was collapsed away); only the new chunk streams.
  apply_render(m, b, { AssistantThinkingChunk = { content = 'B1' } })
  apply_render(m, b, { AssistantThinkingChunk = { content = '\nB2' } })
  local l = lines_of(b)
  check(l[1] == '► ASSISTANT' and l[2] == 'B1' and l[3] == 'B2', "buffer ['label','B1','B2'] after merge")
  check(block.content == 'A1\nA2B1\nB2', 'the MODEL holds the full merged content')
  -- Final collapse yields one indicator.
  render_diff(m, b, T.close_open_elements(m))
  l = lines_of(b)
  check(l[2] == '' and l[3] == '' and l[4] == '', 'single indicator + spacers after final collapse')
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
  check(l[1] == '► ASSISTANT' and l[2] == 'bulk two', 'buffer streams only the un-materialized tail')
  render_diff(m, b, T.close_open_elements(m))
  l = lines_of(b)
  check(l[2] == '' and l[3] == '' and l[4] == '', 'merged run collapsed to one indicator')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

-- ---------------------------------------------------------- tool call

test('tool_call: full lifecycle with preview collapse and nav range', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantToolCallStart = { tool_call_id = 't1', tool_name = 'bash', tool_call_index = 0 } })
  local tc = m.elements[1]
  local l = lines_of(b)
  check(l[1] == '► TOOL' and l[2] == TC_FENCE and l[3] == '', 'label + open fence + blank')

  apply_render(m, b, { AssistantToolCallArgChunk = { tool_call_index = 0, content = 'a1\nb2' } })
  apply_render(m, b, { AssistantToolCallArgChunk = { tool_call_index = 0, content = '\nb3\nb4' } })
  l = lines_of(b)
  check(l[3] == 'a1' and l[4] == 'b2' and l[5] == 'b3' and l[6] == 'b4', 'args stream inside the fence')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')

  -- ToolMessageStart: >2-line args collapse to a preview; fence close; output
  -- fence + blank; output_started is set.
  apply_render(m, b, { ToolMessageStart = { tool_call_id = 't1', tool_args = '' } })
  check(tc.output_started == true and tc.args_collapsed == true, 'output_started + args_collapsed set')
  l = lines_of(b)
  local flat = 'a1\\nb2\\nb3\\nb4'
  check(l[3] == flat, 'args collapsed to the flat preview row')
  check(l[4] == TC_FENCE and l[5] == TC_FENCE and l[6] == '', 'args close fence + output fence + blank')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')

  -- Output streams at the tail.
  apply_render(m, b, { ToolOutputChunk = { tool_call_id = 't1', content = 'out1' } })
  apply_render(m, b, { ToolOutputChunk = { tool_call_id = 't1', content = '\nout2' } })
  l = lines_of(b)
  check(l[6] == 'out1' and l[7] == 'out2', 'output streams after the output fence')

  -- ToolMessageEnd: output fence closes, end_info added.
  apply_render(m, b, { ToolMessageEnd = { tool_call_id = 't1', end_status = 'Succeeded', input_tokens = 3, output_tokens = 4 } })
  l = lines_of(b)
  check(l[8] == TC_FENCE, 'output close fence')
  check(l[9] == '► INFO', 'end_info row after the region')
  local marks = vim.api.nvim_buf_get_extmarks(b, tc_ns_id, 0, -1, { details = true })
  check(#marks == 1 and marks[1][2] == 0 and marks[1][4].end_row == 8, 'tc nav [0,8) exclusive')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('tool_call: collapsed args get the expand-hint virt line', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantToolCallStart = { tool_call_id = 't1', tool_name = 'bash', tool_call_index = 0 } })
  apply_render(m, b, { AssistantToolCallArgChunk = { tool_call_index = 0, content = 'a\nb\nc\nd' } })
  apply_render(m, b, { ToolMessageStart = { tool_call_id = 't1', tool_args = '' } })
  -- width defaults to 80 headless: preview = 7 chars, kept_visual = 1,
  -- visual_count = 4 -> hidden_visual = 3.
  local marks = vim.api.nvim_buf_get_extmarks(b, thinking_ns_id, 0, -1, { details = true })
  local hint = false
  for _, mm in ipairs(marks) do
    if mm[4] and mm[4].virt_lines then
      local line = mm[4].virt_lines[1][1][1]
      if line == '[... press o to expand 3 more lines]' then hint = true end
    end
  end
  check(hint, 'expand-hint virt line on the preview row')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('bulk: content writes are deferred and materialized by updated_all', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantToolCallStart = { tool_call_id = 't1', tool_name = 'bash', tool_call_index = 0 } }, true)
  local l = lines_of(b)
  check(l[2] == TC_FENCE and l[3] == '', 'label + fence + blank rendered at added')
  check(vim.bo[b].modifiable == false, 'modifiable false after added')

  apply_render(m, b, { AssistantToolCallArgChunk = { tool_call_index = 0, content = 'a\nb\nc\nd' } }, true)
  l = lines_of(b)
  check(#l == 3, 'args chunk writes nothing during bulk')
  check(vim.bo[b].modifiable == false, 'modifiable false after skipped chunk')

  -- ToolMessageStart materializes the full args from the model (preview when
  -- long) in one shot.
  apply_render(m, b, { ToolMessageStart = { tool_call_id = 't1', tool_args = '' } }, true)
  l = lines_of(b)
  check(l[3] == 'a\\nb\\nc\\nd', 'full args materialized as the preview row')
  check(l[4] == TC_FENCE and l[5] == TC_FENCE and l[6] == '', 'fences + output blank after materialization')
  check(vim.bo[b].modifiable == false, 'modifiable false after materialization')

  apply_render(m, b, { ToolOutputChunk = { tool_call_id = 't1', content = 'zzz' } }, true)
  l = lines_of(b)
  check(l[6] == '', 'output chunk skipped during bulk')
  check(vim.bo[b].modifiable == false, 'modifiable false after skipped output')
end)

test('parallel: tc1 output lands mid-buffer above tc2, anchors ride the insert', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantToolCallStart = { tool_call_id = 'tc1', tool_name = 'bash', tool_call_index = 0 } })
  apply_render(m, b, { AssistantToolCallStart = { tool_call_id = 'tc2', tool_name = 'grep', tool_call_index = 1 } })
  apply_render(m, b, { ToolMessageStart = { tool_call_id = 'tc1', tool_args = '' } })
  apply_render(m, b, { ToolMessageStart = { tool_call_id = 'tc2', tool_args = '' } })
  local l = lines_of(b)
  check(#l == 12, 'two 6-row regions')
  -- tc1 output streams into its own mid-buffer region.
  apply_render(m, b, { ToolOutputChunk = { tool_call_id = 'tc1', content = 'out1' } })
  apply_render(m, b, { ToolOutputChunk = { tool_call_id = 'tc1', content = '\nout2' } })
  l = lines_of(b)
  check(l[6] == 'out1' and l[7] == 'out2', 'tc1 output above tc2')
  check(l[8] == '► TOOL', 'tc2 label below tc1 output')
  check(l[9] == TC_FENCE and l[13] == '', 'tc2 region intact after the mid-buffer insert')
  local gen = vim.api.nvim_buf_get_extmarks(b, gen_ns_id, 0, -1, {})
  check(#gen == 2 and gen[1][2] == 0 and gen[2][2] == 7, 'anchors: tc1 at 0, tc2 at 7')
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
  check(l[1] == '► SUBAGENT' and l[2] == TC_FENCE and l[3] == '', 'label + fence + blank')

  apply_render(m, b, { SubAgentInputChunk = { tool_call_index = 0, content = '{"task":' } })
  apply_render(m, b, { SubAgentInputChunk = { tool_call_index = 0, content = '"do x"}' } })
  l = lines_of(b)
  check(l[3] == '{"task":"do x"}', 'input streams inside the fence')

  -- AssistantMessageEnd closes the input fence (as in the real protocol).
  apply_render(m, b, { AssistantMessageEnd = {} })
  l = lines_of(b)
  check(l[4] == TC_FENCE and l[5] == '', 'input fence closed + blank output row')

  -- SubAgentStart: running status, output region follows.
  apply_render(m, b, { SubAgentStart = { tool_call_id = 'sa1', conversation_id = 'conv1', description = 'helper' } })
  check(sa.status == 'running' and sa.conversation_id == 'conv1', 'status + conversation set')
  -- Subagent output streams via AssistantMessageChunk (sa_active).
  apply_render(m, b, { AssistantMessageChunk = { content = 'result1' } })
  apply_render(m, b, { AssistantMessageChunk = { content = ' result2' } })
  l = lines_of(b)
  check(l[5] == 'result1 result2', 'output streams below the input region')

  -- SubAgentEnd: final status + error rows.
  apply_render(m, b, { SubAgentEnd = { conversation_id = 'conv1', end_status = 'Failed', error = 'boom', input_tokens = 5, output_tokens = 6 } })
  l = lines_of(b)
  check(l[6] == '' and l[7] == 'Error: boom', 'error rows at the region bottom')
  local marks = vim.api.nvim_buf_get_extmarks(b, sa_ns_id, 0, -1, { details = true })
  check(#marks == 1 and marks[1][2] == 0 and marks[1][4].end_row == 7, 'sa nav [0,7) exclusive')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('subagent: a long input collapses to a preview at SubAgentStart', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { SubAgentInputStart = { tool_call_id = 'sa1', tool_name = 'subagent', tool_call_index = 0 } })
  apply_render(m, b, { SubAgentInputChunk = { tool_call_index = 0, content = '{"a":1,\n"b":2,\n"c":3}' } })
  apply_render(m, b, { AssistantMessageEnd = {} })
  local sa = m.elements[1]
  apply_render(m, b, { SubAgentStart = { tool_call_id = 'sa1', conversation_id = 'c1', description = 'helper' } })
  check(sa.input_collapsed == true, '3-line input collapsed')
  local l = lines_of(b)
  check(l[3] == '{"a":1,\\n"b":2,\\n"c":3}', 'input rendered as the flat preview row')
  check(l[4] == TC_FENCE and l[5] == '', 'fence close + output blank after the preview')
end)

test('subagent: post-flush input chunks are absorbed into the input region at SubAgentStart', function()
  -- Buffer-level mirror of the model regression 'chunks after the settle flush
  -- still accumulate': the flush closes the input fence mid-stream, a later
  -- chunk must still accumulate into el.input, and the SubAgentStart rebuild
  -- must render it INSIDE the input fence — no stray or duplicated rows.
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
  check(l[1] == '► SUBAGENT' and l[2] == TC_FENCE and l[3] == '{"a":1,' and l[4] == '"b":2}'
    and l[5] == TC_FENCE and l[6] == '', 'flush rows: label + open fence + 2 input rows + close fence + blank')
  -- A chunk arriving after the flush accumulates into el.input.
  apply_render(m, b, { SubAgentInputChunk = { tool_call_index = 0, content = ',"c":3}' } })
  check(sa.input == '{"a":1,\n"b":2},"c":3}', 'post-flush chunk accumulated into the model')
  -- AssistantMessageEnd (as in the real protocol) adds nothing visible.
  apply_render(m, b, { AssistantMessageEnd = {} })
  -- SubAgentStart rebuilds the region from full model state: the post-flush
  -- chunk is absorbed back INSIDE the input fence, no stray/duplicated rows.
  apply_render(m, b, { SubAgentStart = { tool_call_id = 'sa1', conversation_id = 'c1', description = 'helper' } })
  l = lines_of(b)
  check(l[1] == '► SUBAGENT' and l[2] == TC_FENCE and l[3] == '{"a":1,' and l[4] == '"b":2},"c":3}'
    and l[5] == TC_FENCE and l[6] == '', 'post-flush chunk absorbed into the input region by the rebuild')
  -- The active subagent streams output at the true tail.
  apply_render(m, b, { AssistantMessageChunk = { content = 'result1' } })
  apply_render(m, b, { AssistantMessageChunk = { content = ' result2' } })
  l = lines_of(b)
  check(l[6] == 'result1 result2', 'output streams at the tail below the input region')
  check(#l == 6, 'no stray rows: exactly the 6-row region')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

-- ------------------------------------------------------- tail append rule

test('assistant message: content appends at the buffer tail below a collapsed block', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantMessageStart = {} })
  apply_render(m, b, { AssistantThinkingChunk = { content = 'from file' } }, true) -- bulk: not written
  render_diff(m, b, T.close_open_elements(m))
  local l = lines_of(b)
  check(l[1] == '► ASSISTANT' and l[2] == '' and l[3] == '' and l[4] == '', 'collapsed block below the label')
  -- A real text chunk appends at the buffer tail (append-only element), even
  -- though the collapsed thinking region sits between the label and the tail.
  apply_render(m, b, { AssistantMessageChunk = { content = ' reply' } })
  l = lines_of(b)
  check(l[1] == '► ASSISTANT' and l[2] == '' and l[3] == '' and l[4] == ' reply',
    "exact rows ['► ASSISTANT','','',' reply']")
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

test('nav extmarks: exclusive end_row tracks each region end', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { UserMessage = { content = 'u1\nu2' } })
  apply_render(m, b, { AssistantMessageStart = {} })
  apply_render(m, b, { SubAgentInputStart = { tool_call_id = 'sa1', tool_call_index = 0 } })
  apply_render(m, b, { AssistantMessageEnd = {} })
  apply_render(m, b, { SubAgentStart = { tool_call_id = 'sa1', conversation_id = 'c1', description = 'd' } })
  apply_render(m, b, { AssistantMessageChunk = { content = 'sub out' } })
  local um = vim.api.nvim_buf_get_extmarks(b, um_ns_id, 0, -1, { details = true })
  local sa = vim.api.nvim_buf_get_extmarks(b, sa_ns_id, 0, -1, { details = true })
  check(#um == 1 and um[1][2] == 0 and um[1][4].end_row == 3, 'um covers label + content, exclusive')
  -- subagent region: label + fence + '' + fence + output = rows 5..9
  check(#sa == 1 and sa[1][2] == 5 and sa[1][4].end_row == 10, 'sa end_row grows with the output')
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
  check(l[1] == '► ASSISTANT' and l[2] == 'hello', 'hand-built delta streams onto the trailing blank')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

-- ------------------------------------------------------- nav / cursor lookup

test('nav extmarks: el_id maps resolve mark -> element and end_row grows with output', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { UserMessage = { content = 'line1\nline2' } }) -- rows 0-2
  local um = m.elements[1]
  apply_render(m, b, { AssistantToolCallStart = { tool_call_id = 't1', tool_name = 'bash', tool_call_index = 0 } })
  local tc = m.elements[2]
  apply_render(m, b, { AssistantToolCallArgChunk = { tool_call_index = 0, content = 'a1\nb2' } })
  apply_render(m, b, { ToolMessageStart = { tool_call_id = 't1', tool_args = '' } })
  apply_render(m, b, { ToolOutputChunk = { tool_call_id = 't1', content = 'out1' } })
  apply_render(m, b, { ToolOutputChunk = { tool_call_id = 't1', content = '\nout2' } })
  apply_render(m, b, { SubAgentInputStart = { tool_call_id = 's1', tool_call_index = 1 } })
  local sa = m.elements[3]
  apply_render(m, b, { AssistantMessageEnd = {} })
  apply_render(m, b, { SubAgentStart = { tool_call_id = 's1', conversation_id = 'c1', description = 'd' } })
  apply_render(m, b, { AssistantMessageChunk = { content = 'sub1' } })
  apply_render(m, b, { AssistantMessageChunk = { content = '\nsub2' } })

  local state = T.get_renderer_state(m)
  local um_marks = vim.api.nvim_buf_get_extmarks(b, um_ns_id, 0, -1, { details = true })
  local tc_marks = vim.api.nvim_buf_get_extmarks(b, tc_ns_id, 0, -1, { details = true })
  local sa_marks = vim.api.nvim_buf_get_extmarks(b, sa_ns_id, 0, -1, { details = true })

  -- user message: end_row == anchor + 1 + #content_lines (exclusive).
  check(#um_marks == 1, 'one um mark')
  check(um_marks[1][2] == 0 and um_marks[1][4].end_row == 3, 'um covers label + 2 content rows, exclusive')
  local um_id = um_marks[1][1]
  check(state.nav[um.id] == um_id and state.nav_ids[um_ns_id][um_id] == um.id, 'um mark resolves to the element via the id maps')

  -- tool call: end_row == the full closed region (label + args fence + args +
  -- args close + output fence + output + output close), grown by the output.
  check(#tc_marks == 1, 'one tc mark')
  check(tc_marks[1][2] == 3, 'tc region starts at its label row')
  check(tc_marks[1][4].end_row == 11, 'tc end_row covers the closed region after streaming')
  local tc_id = tc_marks[1][1]
  check(state.nav[tc.id] == tc_id and state.nav_ids[tc_ns_id][tc_id] == tc.id, 'tc mark resolves to the element via the id maps')

  -- subagent: end_row grows as output streams.
  check(#sa_marks == 1, 'one sa mark')
  check(sa_marks[1][2] == 11, 'sa region starts at its label row')
  check(sa_marks[1][4].end_row == 17, 'sa end_row grew with the streamed output')
  local sa_id = sa_marks[1][1]
  check(state.nav[sa.id] == sa_id and state.nav_ids[sa_ns_id][sa_id] == sa.id, 'sa mark resolves to the element via the id maps')
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
  -- Anchors resolved from gen_ns (rows at use time, never stale).
  local anchors = vim.api.nvim_buf_get_extmarks(b, gen_ns_id, 0, -1, {})
  check(#anchors == 3, 'three element anchors placed')
  check(T.element_at_row(m, b, 0) == um, 'user message at its label row')
  check(T.element_at_row(m, b, 2) == um, 'user message at its last content row')
  check(T.element_at_row(m, b, anchors[2][2]) == tc, 'tool call at its label row')
  check(T.element_at_row(m, b, anchors[3][2]) == sa, 'subagent at its label row')
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

-- ------------------------------------------------------------- mark id reuse

test('thinking: the indicator mark id is preserved across collapse/expand cycles', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantMessageStart = {} })
  apply_render(m, b, { AssistantThinkingChunk = { content = 'L1\nL2' } })
  local block = m.elements[2]
  render_diff(m, b, T.close_open_elements(m))
  local collapsed = thinking_mark_for(b, block.id)
  local mark_id = collapsed and collapsed[1]
  check(mark_id ~= nil, 'indicator mark created on collapse')
  -- Expand: the SAME mark id is reused, so a captured id keeps resolving.
  render_diff(m, b, T.toggle_thinking_element(m, block))
  local expanded = mark_by_id(b, mark_id)
  check(expanded ~= nil and expanded[4].virt_lines ~= nil, 'expanded mark reuses the captured id')
  -- Collapse again via the same captured id: indicator restored, id preserved.
  render_diff(m, b, T.toggle_thinking_element(m, block))
  local recollapsed = mark_by_id(b, mark_id)
  check(recollapsed ~= nil and recollapsed[4].virt_text ~= nil, 'recollapsed indicator reuses the captured id')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('hint marks: the args preview mark id is reused across collapse/expand cycles', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantMessageStart = {} })
  apply_render(m, b, { AssistantToolCallStart = { tool_name = 'bash', tool_call_id = 't1', tool_call_index = 0 } })
  apply_render(m, b, { AssistantToolCallArgChunk = { tool_call_index = 0, content = 'a\nb\nc\nd' } })
  apply_render(m, b, { ToolMessageStart = { tool_call_id = 't1', tool_args = '' } })
  local tc = m.elements[2]
  local marks = vim.api.nvim_buf_get_extmarks(b, thinking_ns_id, 0, -1, { details = true })
  local args_mark = marks[1] and marks[1][1]
  check(args_mark ~= nil, 'args preview mark created on the collapse')
  local el, kind = T.find_marked_element_at(m, b, 4)
  check(el == tc and kind == 'args', 'preview resolves to (tool_call, args)')

  -- Expand: the SAME mark id is recreated as the collapse hint, so a captured
  -- id keeps resolving through the live mark after the rebuild.
  render_diff(m, b, T.toggle_tool_call_args_element(m, tc))
  local expanded = mark_by_id(b, args_mark)
  check(expanded ~= nil, 'expanded collapse hint reuses the captured id')
  check(expanded[4].virt_lines and expanded[4].virt_lines[1][1][1] == '[... press o to collapse]',
    'expanded hint text on the reused mark')
  local el2, kind2 = T.find_marked_element_at(m, b, 5)
  check(el2 == tc and kind2 == 'args', 'expanded content resolves via the live mark')

  -- Collapse again: back to the preview, same id, still resolvable.
  render_diff(m, b, T.toggle_tool_call_args_element(m, tc))
  local recollapsed = mark_by_id(b, args_mark)
  check(recollapsed ~= nil, 'recollapsed preview reuses the captured id')
  local el3, kind3 = T.find_marked_element_at(m, b, 4)
  check(el3 == tc and kind3 == 'args', 'recollapsed preview resolves via the live mark')
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

-- ----------------------------------------------------- output auto-collapse

test('tool output: long result collapses to a preview at ToolMessageEnd, o toggles', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantMessageStart = {} })
  apply_render(m, b, { AssistantToolCallStart = { tool_name = 'read', tool_call_id = 't1', tool_call_index = 0 } })
  apply_render(m, b, { ToolMessageStart = { tool_call_id = 't1', tool_name = 'read' } })
  local tc = m.elements[2]
  apply_render(m, b, { ToolOutputChunk = { tool_call_id = 't1', content = 'o1\n' } })
  apply_render(m, b, { ToolOutputChunk = { tool_call_id = 't1', content = 'o2\no3\no4' } })
  check(tc.output == 'o1\no2\no3\no4', 'output accumulated while streaming')
  check(tc.output_collapsed == false, 'streaming output is expanded')

  -- ToolMessageEnd auto-collapses the 4-line output to a preview row.
  apply_render(m, b, { ToolMessageEnd = { tool_call_id = 't1', end_status = 'Succeeded', input_tokens = 1, output_tokens = 4 } })
  check(tc.output_collapsed == true, 'long output collapsed')
  local l = lines_of(b)
  check(l[1] == '► ASSISTANT' and l[2] == '', 'assistant label intact')
  check(l[3] == '► TOOL' and l[4] == TC_FENCE and l[5] == '', 'tool label + args fence + empty args row')
  check(l[6] == TC_FENCE and l[7] == TC_FENCE, 'args close fence + output fence')
  check(l[8] == 'o1\\no2\\no3\\no4', 'output collapsed to the flat preview row')
  check(l[9] == TC_FENCE, 'output close fence')
  check(l[10] == '► INFO', 'end_info below the tool region')

  -- This tool streamed NO args: the ONLY hint mark is the output preview, so
  -- its id must survive every toggle (the args-less single-mark regression:
  -- a positional reuse scheme would hand it to the args slot and churn the id).
  local marks = vim.api.nvim_buf_get_extmarks(b, thinking_ns_id, 0, -1, { details = true })
  local out_mark_id = nil
  for _, mm in ipairs(marks) do
    if mm[4] and mm[4].virt_lines then
      local line = mm[4].virt_lines[1][1][1]
      if line:find('expand', 1, true) then out_mark_id = mm[1] end
    end
  end
  check(out_mark_id ~= nil, 'output preview mark id captured')

  -- `o` on the preview row resolves the element with kind 'output' -> expand.
  local el, kind = T.find_marked_element_at(m, b, 7) -- 0-indexed preview row
  check(el == tc and kind == 'output', 'preview mark resolves (element, output)')
  render_diff(m, b, T.toggle_tool_output_element(m, tc))
  l = lines_of(b)
  check(l[8] == 'o1' and l[9] == 'o2' and l[10] == 'o3' and l[11] == 'o4' and l[12] == TC_FENCE, 'expanded output rows + close fence')
  local expanded = mark_by_id(b, out_mark_id)
  check(expanded ~= nil and expanded[4].virt_lines ~= nil, 'expanded collapse hint reuses the captured output id')

  -- `o` anywhere in the expanded content resolves and collapses again (the
  -- collapse hint spans the content rows).
  local el2, kind2 = T.find_marked_element_at(m, b, 9) -- an expanded content row
  check(el2 == tc and kind2 == 'output', 'expanded content carries the collapse hint')
  render_diff(m, b, T.toggle_tool_output_element(m, tc))
  l = lines_of(b)
  check(l[8] == 'o1\\no2\\no3\\no4', 'collapsed back to the preview row')
  local recollapsed = mark_by_id(b, out_mark_id)
  check(recollapsed ~= nil and recollapsed[4].virt_lines ~= nil, 'recollapsed preview reuses the captured output id')
  local el3, kind3 = T.find_marked_element_at(m, b, 7)
  check(el3 == tc and kind3 == 'output', 'recollapsed preview resolves via the live mark')

  -- Second toggle cycle: the id must still be stable (no churn per toggle).
  render_diff(m, b, T.toggle_tool_output_element(m, tc))
  check(mark_by_id(b, out_mark_id) ~= nil, 'second expand reuses the captured output id')
  render_diff(m, b, T.toggle_tool_output_element(m, tc))
  local final_mark = mark_by_id(b, out_mark_id)
  check(final_mark ~= nil and final_mark[4].virt_lines ~= nil, 'second collapse reuses the captured output id')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('subagent output: long result collapses at SubAgentEnd, o toggles', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { SubAgentInputStart = { tool_call_id = 'sa1', tool_call_index = 0 } })
  apply_render(m, b, { AssistantMessageEnd = {} })
  apply_render(m, b, { SubAgentStart = { tool_call_id = 'sa1', conversation_id = 'c1', description = 'helper' } })
  local sa = m.elements[1]
  apply_render(m, b, { AssistantMessageChunk = { content = 's1\n' } })
  apply_render(m, b, { AssistantMessageChunk = { content = 's2\ns3\ns4' } })
  check(sa.output == 's1\ns2\ns3\ns4' and sa.output_collapsed == false, 'streaming expanded')
  apply_render(m, b, { SubAgentEnd = { conversation_id = 'c1', end_status = 'Succeeded', input_tokens = 1, output_tokens = 4 } })
  check(sa.output_collapsed == true, 'long output collapsed at SubAgentEnd')
  local l = lines_of(b)
  check(l[5] == 's1\\ns2\\ns3\\ns4', 'output collapsed to the preview row')
  local el, kind = T.find_marked_element_at(m, b, 4)
  check(el == sa and kind == 'output', 'subagent output preview mark resolves')
  render_diff(m, b, T.toggle_tool_output_element(m, sa))
  l = lines_of(b)
  check(l[5] == 's1' and l[6] == 's2' and l[7] == 's3' and l[8] == 's4', 'expanded subagent output rows')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('o dispatch: expanded args content carries a collapse hint; label row is the detail anchor', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantMessageStart = {} })
  apply_render(m, b, { AssistantToolCallStart = { tool_name = 'write', tool_call_id = 't1', tool_call_index = 0 } })
  apply_render(m, b, { AssistantToolCallArgChunk = { tool_call_index = 0, content = 'a\nb\nc\nd' } })
  local tc = m.elements[2]
  apply_render(m, b, { ToolMessageStart = { tool_call_id = 't1', tool_name = 'write' } })
  check(tc.args_collapsed == true, 'args collapsed at ToolMessageStart')

  -- `o` on the args preview row -> (el, 'args').
  local el, kind = T.find_marked_element_at(m, b, 4)
  check(el == tc and kind == 'args', 'args preview mark resolves to (el, args)')

  -- Expand via the reducer; the content now carries a collapse hint spanning
  -- its rows, so `o` on ANY content row collapses (never opens the detail).
  render_diff(m, b, T.toggle_tool_call_args_element(m, tc))
  local el2, kind2 = T.find_marked_element_at(m, b, 5) -- an args content row
  check(el2 == tc and kind2 == 'args', 'expanded args carry the collapse hint')
  render_diff(m, b, T.toggle_tool_call_args_element(m, tc))
  check(tc.args_collapsed == true, 'collapsed back to the preview')

  -- The label row resolves the element via element_at_row (the detail-view
  -- branch checks anchor == cursor there), and the content rows resolve too
  -- but are covered by the marks above, so the detail opens only on the label.
  local label_el = T.element_at_row(m, b, 3) -- 0-indexed label row
  check(label_el == tc, 'element_at_row resolves the label row to the tool call')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('label overlay: status changes reuse one mark, no stacking', function()
  -- Regression: every updated_all used to place a NEW label overlay extmark;
  -- the old marks drifted to the region end and stacked there (visible as
  -- garbage token/description text on the end_info row). Status cycles must
  -- move ONE mark, not accumulate.
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
  local marks = vim.api.nvim_buf_get_extmarks(b, ns, 0, -1, { details = true })
  local sa_overlays = 0
  for _, mk in ipairs(marks) do
    local det = mk[4] or {}
    if det.virt_text then
      for _, item in ipairs(det.virt_text) do
        if (item[1] or ''):find('>>> SUB-AGENT:', 1, true) then sa_overlays = sa_overlays + 1 end
      end
    end
  end
  check(sa_overlays == 1, 'exactly one subagent label overlay after many status changes')
  local l = lines_of(b)
  check(l[1] == '► SUBAGENT', 'subagent label real row intact')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

-- ------------------------------------------------ highlight-mark bounds

-- Count the thinking_ns content-highlight marks (hl_group TCodeThinking).
-- Indicator/collapse-hint marks carry virt_text/virt_lines, never an
-- hl_group, so the filter isolates exactly the per-row content highlights.
local function count_thinking_hl(b)
  local n = 0
  local marks = vim.api.nvim_buf_get_extmarks(b, thinking_ns_id, 0, -1, { details = true })
  for _, mm in ipairs(marks) do
    if mm[4] and mm[4].hl_group == 'TCodeThinking' then n = n + 1 end
  end
  return n
end

-- Rows covered by the TCodeThinking highlights, sorted (validates the counting
-- method against the actual render: one mark per highlighted content row).
local function thinking_hl_rows(b)
  local rows = {}
  local marks = vim.api.nvim_buf_get_extmarks(b, thinking_ns_id, 0, -1, { details = true })
  for _, mm in ipairs(marks) do
    if mm[4] and mm[4].hl_group == 'TCodeThinking' then rows[#rows + 1] = mm[2] end
  end
  table.sort(rows)
  return rows
end

-- Count the display-ns TCodeToolArgs highlights (tool args / subagent input).
-- Label overlays in the same ns carry virt_text, never an hl_group.
local function count_args_hl(b)
  local n = 0
  local marks = vim.api.nvim_buf_get_extmarks(b, ns, 0, -1, { details = true })
  for _, mm in ipairs(marks) do
    if mm[4] and mm[4].hl_group == 'TCodeToolArgs' then n = n + 1 end
  end
  return n
end

test('hl bound: thinking block keeps one mark per row across collapse/expand cycles', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantMessageStart = {} })
  apply_render(m, b, { AssistantThinkingChunk = { content = 'L1\nL2\nL3' } })
  local block = m.elements[2]
  -- Counting method validated against the render: the three content rows each
  -- carry exactly one TCodeThinking mark on their own row.
  check(count_thinking_hl(b) == 3, 'streamed content highlighted once per row')
  local rows = thinking_hl_rows(b)
  check(rows[1] == 1 and rows[2] == 2 and rows[3] == 3, 'marks sit on the content rows 1-3')
  -- First collapse via the settle flush (open -> collapsed), then toggle
  -- collapse <-> expand: the count must not grow (before the fix, marks
  -- replaced by the rebuild slid past the region and stacked).
  render_diff(m, b, T.close_open_elements(m))
  check(count_thinking_hl(b) == 0, 'collapse removes every content highlight')
  for i = 1, 5 do
    render_diff(m, b, T.toggle_thinking_element(m, block)) -- collapsed -> expanded
    check(count_thinking_hl(b) == 3, 'expand places one mark per content row')
    render_diff(m, b, T.toggle_thinking_element(m, block)) -- expanded -> collapsed
    check(count_thinking_hl(b) == 0, 'collapse removes every content highlight')
  end
  -- Merge reopen after a collapse: the tail renders fresh, then streaming must
  -- not stack duplicate marks on the join row.
  apply_render(m, b, { AssistantThinkingChunk = { content = 'M1' } }) -- merge reopen
  check(count_thinking_hl(b) == 1, 'merge-reopened tail highlighted once')
  for _ = 1, 50 do
    apply_render(m, b, { AssistantThinkingChunk = { content = 'x' } })
  end
  check(count_thinking_hl(b) == 1, '50 newline-less chunks after the reopen add no marks')
  check(lines_of(b)[2] == 'M1' .. string.rep('x', 50), 'chunks joined the reopened tail row')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('hl dedup: newline-less chunks stack no marks on the thinking join row', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantMessageStart = {} })
  apply_render(m, b, { AssistantThinkingChunk = { content = 'A\nB' } })
  local block = m.elements[2]
  check(count_thinking_hl(b) == 2, 'two content rows highlighted')
  local rows = thinking_hl_rows(b)
  check(rows[1] == 1 and rows[2] == 2, 'marks on the A and B rows')
  -- 50 single-char chunks all join the 'B' row: distinct content rows ==
  -- highlight marks, the join row keeps exactly one.
  for _ = 1, 50 do
    apply_render(m, b, { AssistantThinkingChunk = { content = 'x' } })
  end
  check(count_thinking_hl(b) == 2, '50 newline-less chunks add no marks')
  local join_row_marks = 0
  local marks = vim.api.nvim_buf_get_extmarks(b, thinking_ns_id, 0, -1, { details = true })
  for _, mm in ipairs(marks) do
    if mm[4] and mm[4].hl_group == 'TCodeThinking' and mm[2] == 2 then join_row_marks = join_row_marks + 1 end
  end
  check(join_row_marks == 1, 'join row carries exactly one highlight mark')
  check(lines_of(b)[2] == 'A' and lines_of(b)[3] == 'B' .. string.rep('x', 50), 'chunks joined the B row')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('hl bound: tool args highlights stay bounded across permission and toggle cycles', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  apply_render(m, b, { AssistantToolCallStart = { tool_call_id = 't1', tool_name = 'bash', tool_call_index = 0 } })
  apply_render(m, b, { AssistantToolCallArgChunk = { tool_call_index = 0, content = 'a\nb\nc\nd' } })
  local tc = m.elements[1]
  apply_render(m, b, { ToolMessageStart = { tool_call_id = 't1', tool_args = '' } })
  -- Args collapsed to the preview row: exactly one TCodeToolArgs mark.
  check(count_args_hl(b) == 1, 'collapsed args carry one preview highlight')
  local marks = vim.api.nvim_buf_get_extmarks(b, ns, 0, -1, { details = true })
  local on_preview = false
  for _, mm in ipairs(marks) do
    if mm[4] and mm[4].hl_group == 'TCodeToolArgs' and mm[2] == 2 then on_preview = true end
  end
  check(on_preview, 'preview highlight on the preview row 2')
  -- Permission cycles rebuild the region: the count must stay 1, not grow
  -- (before the fix, each rebuild's marks slid past the region and stacked).
  for _ = 1, 10 do
    apply_render(m, b, { ToolRequestPermission = { tool_call_id = 't1' } })
    check(count_args_hl(b) == 1, 'args highlight count stable at permission')
    apply_render(m, b, { ToolPermissionApproved = { tool_call_id = 't1' } })
    check(count_args_hl(b) == 1, 'args highlight count stable after approval')
  end
  -- Args expand/collapse toggles: 4 rows expanded, 1 collapsed, never more.
  for i = 1, 5 do
    render_diff(m, b, T.toggle_tool_call_args_element(m, tc))
    check(count_args_hl(b) == 4, 'expanded args carry four row highlights')
    render_diff(m, b, T.toggle_tool_call_args_element(m, tc))
    check(count_args_hl(b) == 1, 'recollapsed args carry one preview highlight')
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
  check(count_args_hl(b) == 2, 'two args rows highlighted')
  -- 50 single-char chunks join the 'b' row: distinct content rows == marks.
  for _ = 1, 50 do
    apply_render(m, b, { AssistantToolCallArgChunk = { tool_call_index = 0, content = 'x' } })
  end
  check(count_args_hl(b) == 2, '50 newline-less chunks add no marks')
  local join_row_marks = 0
  local marks = vim.api.nvim_buf_get_extmarks(b, ns, 0, -1, { details = true })
  for _, mm in ipairs(marks) do
    if mm[4] and mm[4].hl_group == 'TCodeToolArgs' and mm[2] == 3 then join_row_marks = join_row_marks + 1 end
  end
  check(join_row_marks == 1, 'join row carries exactly one highlight mark')
  check(lines_of(b)[3] == 'a' and lines_of(b)[4] == 'b' .. string.rep('x', 50), 'chunks joined the b row')
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
  check(l[1] == '[Retrying... (attempt 1/3) -- Failed to get access token: {]', 'header row with the first reason line')
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
  check(l[3] == '► ASSISTANT' and l[4] == '', 'assistant label + blank rendered')
  -- Each retry block is a 5-row header + reason body (5 reason lines each).
  check(l[5]:find('Retrying%.%.%. %(attempt 1/3%)', 1) ~= nil, 'first retry header')
  check(l[10]:find('Retrying%.%.%. %(attempt 2/3%)', 1) ~= nil, 'second retry header')
  check(l[15]:find('Retrying%.%.%. %(attempt 3/3%)', 1) ~= nil, 'third retry header')
  check(l[20] == '► INFO', 'end info row')
  check(l[21] == 'Error: Failed to get access token: OpenAI token refresh failed (401 Unauthorized): {', 'end error first line')
  check(l[25] == '}', 'end error last line')
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

test('labels: multi-line tool_name / subagent description never reach virt_text', function()
  -- Wire-derived label text is rendered as overlay virt_text, which must stay
  -- on one line; a '\n' in the tool_name or description must be collapsed.
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  local ok, err = pcall(apply_render, m, b,
    { AssistantToolCallStart = { tool_name = 'read\nscript', tool_call_id = 't1', tool_call_index = 0 } })
  check(ok, 'multi-line tool_name renders without error: ' .. tostring(err))
  local l = lines_of(b)
  check(l[1] == '► TOOL' and l[2] == TC_FENCE, 'tool region rows intact')

  local m2 = T.reset_model()
  local b2 = new_buf()
  seed(b2, { '' })
  apply_render(m2, b2, { SubAgentInputStart = { tool_call_id = 's', tool_call_index = 0 } })
  local ok2, err2 = pcall(apply_render, m2, b2,
    { SubAgentStart = { tool_call_id = 's', conversation_id = 'c1', description = 'multi\nline' } })
  check(ok2, 'multi-line subagent description renders without error: ' .. tostring(err2))
  local l2 = lines_of(b2)
  check(l2[1] == '► SUBAGENT' and l2[2] == TC_FENCE, 'subagent region rows intact')
  check(vim.bo[b].modifiable == false and vim.bo[b2].modifiable == false, 'buffers non-modifiable')
end)
