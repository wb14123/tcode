-- Regression tests for the three renderer bugs the display rewrite fixed, plus
-- the integer row-map navigation contract. Every assertion is against real
-- buffer lines (nvim_buf_get_lines), the row map, and the pure action_at
-- lookup — never extmarks. Chrome (labels, hints, token lines) is real text.

local TC_FENCE = string.rep('`', 10)

local function row_of(m, el)
  return T.get_renderer_state(m).rows[el.id]
end

local function last_thinking(m)
  local el = nil
  for _, e in ipairs(m.elements) do
    if e.type == 'thinking_block' then el = e end
  end
  return el
end

test('bug 1: a collapsed thinking block is one real navigable row; merged reopen keeps full content', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  windowed_render(b, { AssistantMessageStart = {} }, false)
  windowed_render(b, { AssistantThinkingChunk = { content = 'A1\nA2' } }, false)
  local block = last_thinking(m)
  T.collapse_thinking(m, block, b, ns)
  local l = lines_of(b)
  check(#l == 2, 'collapsed block is exactly one real row below the label')
  check(l[2] == '► [Thinking... press o to expand]', 'the collapsed row is the single real hint line')
  local r = row_of(m, block)
  check(r.start_row == 1 and r.height == 1, 'row map: collapsed block at [1, 2)')
  local el, off = T.element_at_row_full(m, b, 1)
  check(el == block and off == 0, 'the collapsed row resolves to (block, offset 0)')
  check(T.action_at(block, 0) == 'thinking', 'o on the collapsed row resolves to a thinking toggle')
  -- A later run merges into the SAME element and reopens it in place; the
  -- full merged content must be visible — no previous block lost.
  windowed_render(b, { AssistantThinkingChunk = { content = 'B1' } }, false)
  windowed_render(b, { AssistantThinkingChunk = { content = '\nB2' } }, false)
  l = lines_of(b)
  check(l[1] == '► ASSISTANT' and l[2] == 'A1' and l[3] == 'A2B1' and l[4] == 'B2',
    'merged reopen renders the FULL content, previous run included')
  check(T.content_of(block, 'content') == 'A1\nA2B1\nB2', 'the model holds both runs in one element')
  r = row_of(m, block)
  check(r.start_row == 1 and r.height == 3, 'row map: merged block at [1, 4)')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('bug 2: assistant content after a tool call + end_info lands on its own rows', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  windowed_render(b, { AssistantMessageStart = {} }, false)
  local am = m.elements[1]
  windowed_render(b, { AssistantToolCallStart = { tool_call_id = 't1', tool_name = 'bash', tool_call_index = 0 } }, false)
  local tc = m.elements[2]
  windowed_render(b, { AssistantToolCallArgChunk = { tool_call_index = 0, content = 'arg' } }, false)
  windowed_render(b, { ToolMessageStart = { tool_call_id = 't1', tool_args = '' } }, false)
  windowed_render(b, { ToolOutputChunk = { tool_call_id = 't1', content = 'res' } }, false)
  windowed_render(b, { ToolMessageEnd = { tool_call_id = 't1', end_status = 'Succeeded', input_tokens = 1, output_tokens = 2 } }, false)
  local info = m.elements[#m.elements]
  check(row_of(m, info).start_row == 10 and row_of(m, info).height == 1,
    'end_info token row sits right after the tool region')
  local before = lines_of(b)
  check(before[#before] == '► [TOOL: 1 in / 2 out tokens]', 'token line holds exactly the token text')
  -- The assistant message is NOT the buffer tail (a tool call + end_info sit
  -- below it); its streamed text must land on its own rows, never on the
  -- token line.
  windowed_render(b, { AssistantMessageChunk = { content = ' reply' } }, false)
  local l = lines_of(b)
  check(l[2] == ' reply', 'assistant content on its own rows (below its label)')
  check(l[#l] == '► [TOOL: 1 in / 2 out tokens]', 'end_info token line intact at the bottom')
  check(l[#l]:find('reply', 1, true) == nil, 'no assistant text after the token line')
  check(row_of(m, am).start_row == 0 and row_of(m, am).height == 2, 'assistant grew in place to [0, 2)')
  check(row_of(m, tc).start_row == 2, 'tool call shifted down below the assistant content')
  check(row_of(m, info).start_row == 11, 'end_info shifted to the new tail')
  local el, off = T.element_at_row_full(m, b, 11)
  check(el == info and off == 0 and T.action_at(info, 0) == nil, 'token row resolves to (info, 0), no action')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('bug 3: the SUB-AGENT label is real buffer text after a region rebuild', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  windowed_render(b, { SubAgentInputStart = { tool_call_id = 'sa1', tool_call_index = 0 } }, false)
  windowed_render(b, { SubAgentInputChunk = { tool_call_index = 0, content = '{"task":"x"}' } }, false)
  windowed_render(b, { AssistantMessageEnd = {} }, false)
  windowed_render(b, { SubAgentStart = { tool_call_id = 'sa1', conversation_id = 'c1', description = 'helper' } }, false)
  windowed_render(b, { AssistantMessageChunk = { content = 'sub result' } }, false)
  local sa = m.elements[1]
  -- SubAgentEnd rebuilds the element (status + token counters): the label is
  -- REAL buffer text, so it must survive the rebuild as a plain line.
  windowed_render(b, { SubAgentEnd = { conversation_id = 'c1', end_status = 'Succeeded', input_tokens = 5, output_tokens = 6 } }, false)
  local l = lines_of(b)
  check(l[1] == '► SUB-AGENT: [done]  [5 in / 6 out]  helper',
    'label rebuilt as real text with the final status + tokens')
  check(l[1]:match('^► ') ~= nil and l[1]:find('SUB%-AGENT', 1) ~= nil,
    'label line matches the SUB-AGENT chrome prefix')
  local label_rows = 0
  for _, line in ipairs(l) do
    if line:find('SUB%-AGENT', 1) then label_rows = label_rows + 1 end
  end
  check(label_rows == 1, 'exactly one subagent label row after the rebuild')
  check(l[#l - 1] == 'sub result' and l[#l] == TC_FENCE, 'streamed output row + close fence below the input region')
  local r = row_of(m, sa)
  check(r.start_row == 0 and r.height == 9, 'row map: subagent region at [0, 9)')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('navigation: element_at_row + action_at resolve every chrome/content row', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  windowed_render(b, { UserMessage = { content = 'u1' } }, false)
  local um = m.elements[1]
  windowed_render(b, { AssistantMessageStart = {} }, false)
  local am = m.elements[2]
  windowed_render(b, { AssistantThinkingChunk = { content = 't1\nt2' } }, false)
  local block = m.elements[3]
  -- Streaming (open) thinking content is not navigable.
  local el, off = T.element_at_row_full(m, b, 3)
  check(el == block and off == 0 and T.action_at(block, 0) == nil,
    'open streaming thinking content -> nil')
  windowed_render(b, { AssistantToolCallStart = { tool_call_id = 't1', tool_name = 'bash', tool_call_index = 0 } }, false)
  local tc = m.elements[4]
  windowed_render(b, { AssistantToolCallArgChunk = { tool_call_index = 0, content = 'a1\na2\na3\na4' } }, false)
  windowed_render(b, { ToolMessageStart = { tool_call_id = 't1', tool_args = '' } }, false)
  windowed_render(b, { ToolOutputChunk = { tool_call_id = 't1', content = 'o1\no2\no3' } }, false)
  windowed_render(b, { ToolMessageEnd = { tool_call_id = 't1', end_status = 'Succeeded', input_tokens = 1, output_tokens = 2 } }, false)
  local info = m.elements[5]
  windowed_render(b, { SubAgentInputStart = { tool_call_id = 'sa1', tool_call_index = 0 } }, false)
  windowed_render(b, { SubAgentInputChunk = { tool_call_index = 0, content = 'i1\ni2\ni3' } }, false)
  windowed_render(b, { AssistantMessageEnd = {} }, false)
  windowed_render(b, { SubAgentStart = { tool_call_id = 'sa1', conversation_id = 'c1', description = 'helper' } }, false)
  windowed_render(b, { AssistantMessageChunk = { content = 'so1\nso2\nso3' } }, false)
  windowed_render(b, { SubAgentEnd = { conversation_id = 'c1', end_status = 'Succeeded', input_tokens = 1, output_tokens = 2 } }, false)
  local sa = m.elements[6]

  -- Expected element per buffer row (row map order) and the action_at intent
  -- each chrome/content row must resolve to. Layout (0-indexed):
  --   user label/content; assistant label; collapsed thinking hint; tool
  --   label/Param/fence/4 args/fence/Result/fence/3 output/fence; end_info
  --   token line; subagent label/Input/fence/3 input/fence/Output/fence/
  --   3 output/fence. Every tool/subagent chrome or content row resolves to
  --   'detail'; the collapsed thinking hint toggles; end_info and the
  --   user/assistant rows resolve to nothing.
  local row_element = {
    um, um, am, block,                          -- rows 0-3
    tc, tc, tc, tc, tc, tc, tc, tc,             -- rows 4-11
    tc, tc, tc, tc, tc, tc,                     -- rows 12-17
    info,                                       -- row 18
    sa, sa, sa, sa, sa, sa, sa, sa, sa, sa,     -- rows 19-28
    sa, sa, sa,                                 -- rows 29-31
  }
  local expected_action = {
    [0] = nil, [1] = nil, [2] = nil, [3] = 'thinking',
    [4] = 'detail', [5] = 'detail', [6] = 'detail', [7] = 'detail',
    [8] = 'detail', [9] = 'detail', [10] = 'detail', [11] = 'detail',
    [12] = 'detail', [13] = 'detail', [14] = 'detail', [15] = 'detail',
    [16] = 'detail', [17] = 'detail',
    [18] = nil,
    [19] = 'detail', [20] = 'detail', [21] = 'detail', [22] = 'detail',
    [23] = 'detail', [24] = 'detail', [25] = 'detail', [26] = 'detail',
    [27] = 'detail', [28] = 'detail', [29] = 'detail', [30] = 'detail',
    [31] = 'detail',
  }
  local all_ok = true
  local detail = {}
  for row = 0, #lines_of(b) - 1 do
    local e, off = T.element_at_row_full(m, b, row)
    local want_el = row_element[row + 1]
    if e ~= want_el or off ~= row - row_of(m, want_el).start_row then
      all_ok = false
      detail[#detail + 1] = ('row %d: element %s off %s'):format(row, tostring(e and e.type), tostring(off))
    end
    local act = e and T.action_at(e, off)
    if act ~= expected_action[row] then
      all_ok = false
      detail[#detail + 1] = ('row %d: action %s want %s'):format(row, tostring(act), tostring(expected_action[row]))
    end
  end
  check(all_ok, 'every row resolves to the right element/offset/action'
    .. (#detail > 0 and (' (' .. table.concat(detail, '; ') .. ')') or ''))
  -- The wrapped element_at_row preserves `== el` identity.
  check(T.element_at_row(m, b, 0) == um, 'user message label row resolves to the same element')
  check(T.element_at_row(m, b, 2) == am, 'assistant label row resolves to the same element')
  check(T.element_at_row(m, b, 4) == tc, 'tool label row resolves to the same element')
  check(T.element_at_row(m, b, 18) == info, 'end_info row resolves to the same element')
  check(T.element_at_row(m, b, 19) == sa, 'subagent label row resolves to the same element')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('regression: the 5-line tail cap never loses the row map', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  windowed_render(b, { AssistantToolCallStart = { tool_call_id = 't1', tool_name = 'bash', tool_call_index = 0 } }, false)
  windowed_render(b, { ToolMessageStart = { tool_call_id = 't1', tool_args = '' } }, false)
  local tc = m.elements[1]
  for i = 1, 10 do windowed_render(b, { ToolOutputChunk = { tool_call_id = 't1', content = 'o' .. i .. '\n' } }, false) end
  local l = lines_of(b)
  check(#l == 8, 'streaming region stays at 8 rows (label + Result + fence + 5 content)')
  check(l[4] == 'o6' and l[8] == 'o10', 'exactly the last 5 of 10 lines are shown')
  local r = row_of(m, tc)
  check(r.start_row == 0 and r.height == 8, 'row map: tool at [0, 8)')
  local el, off = T.element_at_row_full(m, b, 7)
  check(el == tc and off == 7 and T.action_at(tc, 7) == 'detail', 'last streaming row resolves to (tc, 7) detail')
  windowed_render(b, { ToolMessageEnd = { tool_call_id = 't1', end_status = 'Succeeded', input_tokens = 1, output_tokens = 10 } }, false)
  local info = m.elements[2]
  check(row_of(m, tc).height == 9, 'closed tool region is 9 rows')
  check(row_of(m, info).start_row == 9 and row_of(m, info).height == 1, 'end_info at [9, 10)')
  windowed_render(b, { UserMessage = { content = 'after' } }, false)
  local um = m.elements[3]
  check(row_of(m, um).start_row == 10, 'the user message lands after the tool region')
  l = lines_of(b)
  check(l[9] == TC_FENCE and l[10] == '► [TOOL: 1 in / 10 out tokens]' and l[11] == '► USER' and l[12] == 'after',
    'buffer rows: close fence + end_info + user message')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('regression: o opens the detail path from any tool row (stubbed shell-out)', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  windowed_render(b, { AssistantToolCallStart = { tool_call_id = 't1', tool_name = 'bash', tool_call_index = 0 } }, false)
  windowed_render(b, { AssistantToolCallArgChunk = { tool_call_index = 0, content = '{"a":1}' } }, false)
  windowed_render(b, { ToolMessageStart = { tool_call_id = 't1', tool_args = '' } }, false)
  windowed_render(b, { ToolOutputChunk = { tool_call_id = 't1', content = 'res' } }, false)
  -- A real window on the buffer so keymap_o's nvim_win_get_cursor resolves.
  local win = vim.api.nvim_open_win(b, false, { relative = 'editor', width = 40, height = 20, row = 1, col = 1 })
  vim.api.nvim_set_current_win(win)
  local orig_system = vim.fn.system
  local orig_exe = M.exe_path
  local orig_session = M.session_id
  M.exe_path = '/fake/tcode'
  M.session_id = 'sess1'
  local calls = {}
  vim.fn.system = function(cmd) calls[#calls + 1] = cmd; return '' end
  local errors = {}
  for _, row in ipairs({ 0, 1, 6 }) do
    vim.api.nvim_win_set_cursor(win, { row + 1, 0 })
    local ok, err = pcall(T.keymap_o, m, b, ns)
    if not ok then errors[#errors + 1] = err end
  end
  -- Restore BEFORE any check: later tests must never see the stub.
  vim.fn.system = orig_system
  M.exe_path = orig_exe
  M.session_id = orig_session
  vim.api.nvim_win_close(win, true)
  check(#errors == 0, 'keymap_o succeeds from every tool row')
  check(#calls == 3, 'one open-tool-call shell-out per row')
  for _, cmd in ipairs(calls) do
    check(cmd:find('open%-tool%-call', 1) ~= nil, 'shell-out targets open-tool-call')
  end
  -- A pending subagent (conversation_id still nil) is a silent no-op.
  local m2 = T.reset_model()
  local b2 = new_buf()
  seed(b2, { '' })
  windowed_render(b2, { SubAgentInputStart = { tool_call_id = 's1', tool_call_index = 0 } }, false)
  local win2 = vim.api.nvim_open_win(b2, false, { relative = 'editor', width = 40, height = 20, row = 1, col = 1 })
  vim.api.nvim_set_current_win(win2)
  vim.api.nvim_win_set_cursor(win2, { 1, 0 })
  M.exe_path = '/fake/tcode'
  M.session_id = 'sess1'
  local calls2 = {}
  vim.fn.system = function(cmd) calls2[#calls2 + 1] = cmd; return '' end
  local ok2, err2 = pcall(T.keymap_o, m2, b2, ns)
  vim.fn.system = orig_system
  M.exe_path = orig_exe
  M.session_id = orig_session
  vim.api.nvim_win_close(win2, true)
  check(ok2, 'pending subagent: keymap_o returns without error' .. (ok2 and '' or (' (' .. tostring(err2) .. ')')))
  check(#calls2 == 0, 'pending subagent: no shell-out (silent no-op)')
  check(vim.bo[b].modifiable == false and vim.bo[b2].modifiable == false, 'buffers non-modifiable')
end)

test('regression: the subagent output fence closes only at SubAgentEnd', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  windowed_render(b, { SubAgentInputStart = { tool_call_id = 'sa1', tool_call_index = 0 } }, false)
  windowed_render(b, { AssistantMessageEnd = {} }, false)
  windowed_render(b, { SubAgentStart = { tool_call_id = 'sa1', conversation_id = 'c1', description = 'helper' } }, false)
  windowed_render(b, { AssistantMessageChunk = { content = 'line1\nline2\nline3' } }, false)
  local sa = m.elements[1]
  local l = lines_of(b)
  check(l[#l] == 'line3', 'output streams without a close fence while the subagent is active')
  check(#l == 6, 'streaming subagent renders 6 rows (label + Output + fence + 3 lines)')
  windowed_render(b, { SubAgentEnd = { conversation_id = 'c1', end_status = 'Succeeded', input_tokens = 1, output_tokens = 2 } }, false)
  l = lines_of(b)
  check(l[#l] == TC_FENCE, 'close fence appears only after SubAgentEnd')
  check(#l == 7, 'close fence adds exactly one row')
  check(row_of(m, sa).height == #l, 'row map height matches the buffer rows')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('regression: no expand/collapse hint rows exist for tool/subagent in any state', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  windowed_render(b, { AssistantToolCallStart = { tool_call_id = 't1', tool_name = 'bash', tool_call_index = 0 } }, false)
  windowed_render(b, { AssistantToolCallArgChunk = { tool_call_index = 0, content = 'a\nb\nc\nd\ne\nf\ng' } }, false)
  windowed_render(b, { ToolMessageStart = { tool_call_id = 't1', tool_args = '' } }, false)
  windowed_render(b, { ToolOutputChunk = { tool_call_id = 't1', content = 'r1\nr2\nr3\nr4\nr5\nr6' } }, false)
  windowed_render(b, { ToolMessageEnd = { tool_call_id = 't1', end_status = 'Succeeded' } }, false)
  windowed_render(b, { SubAgentInputStart = { tool_call_id = 'sa1', tool_call_index = 0 } }, false)
  windowed_render(b, { SubAgentInputChunk = { tool_call_index = 0, content = 'i1\ni2\ni3\ni4\ni5\ni6' } }, false)
  windowed_render(b, { AssistantMessageEnd = {} }, false)
  windowed_render(b, { SubAgentStart = { tool_call_id = 'sa1', conversation_id = 'c1', description = 'helper' } }, false)
  windowed_render(b, { AssistantMessageChunk = { content = 'o1\no2\no3\no4\no5\no6' } }, false)
  windowed_render(b, { SubAgentEnd = { conversation_id = 'c1', end_status = 'Succeeded' } }, false)
  for _, line in ipairs(lines_of(b)) do
    check(line:find('press o to', 1, true) == nil, 'no expand/collapse hint row anywhere')
  end
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

-- ---------------------------------------------------- attached thinking
-- A thinking block created while the model tail is an assistant_message is
-- ATTACHED to that am: its rows live INSIDE the am's region at the arrival
-- position, so the display order matches the wire order (thinking before
-- response). The am's region is [label] + attached blocks + content,
-- contiguous; an attached block's row entry is a sub-range inside it.

test('attached: common case renders [label, thinking, response] in arrival order', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  windowed_render(b, { AssistantMessageStart = {} }, false)
  local am = m.elements[1]
  windowed_render(b, { AssistantThinkingChunk = { content = 'T1\nT2' } }, false)
  local block = m.elements[2]
  -- The response chunk collapses the open thinking block; the reply lands
  -- below it INSIDE the am region (arrival order).
  windowed_render(b, { AssistantMessageChunk = { content = ' reply' } }, false)
  local l = lines_of(b)
  check(l[1] == '► ASSISTANT' and l[2] == '► [Thinking... press o to expand]' and l[3] == ' reply',
    'arrival order [label, collapsed thinking hint, response]')
  local r = row_of(m, am)
  check(r.start_row == 0 and r.height == 3, 'row map: am region [0, 3) = label + attached block + reply')
  local br = row_of(m, block)
  check(br.start_row == 1 and br.height == 1, 'row map: attached collapsed block sub-entry at [1, 2)')
  local el, off = T.element_at_row_full(m, b, 1)
  check(el == block and off == 0, 'attached block row resolves to (block, offset 0)')
  el, off = T.element_at_row_full(m, b, 2)
  check(el == am and off == 2, 'am content row resolves to (am, offset 2)')
  check(T.action_at(block, 0) == 'thinking', 'attached collapsed block still toggles thinking')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('attached: interleaved thinking/response chunks render in exact arrival order', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  windowed_render(b, { AssistantMessageStart = {} }, false)
  local am = m.elements[1]
  windowed_render(b, { AssistantThinkingChunk = { content = 'block1' } }, false)
  local block1 = m.elements[2]
  windowed_render(b, { AssistantMessageChunk = { content = ' resp1' } }, false)
  windowed_render(b, { AssistantThinkingChunk = { content = 'block2' } }, false)
  local block2 = m.elements[3]
  windowed_render(b, { AssistantMessageChunk = { content = ' resp2' } }, false)
  local l = lines_of(b)
  check(l[1] == '► ASSISTANT' and l[2] == '► [Thinking... press o to expand]'
    and l[3] == ' resp1' and l[4] == '► [Thinking... press o to expand]' and l[5] == ' resp2',
    'exact arrival order: label, block1 hint, resp1, block2 hint, resp2')
  local r = row_of(m, am)
  check(r.start_row == 0 and r.height == 5, 'row map: am region [0, 5) folds both blocks + both replies')
  local b1 = row_of(m, block1)
  local b2 = row_of(m, block2)
  check(b1.start_row == 1 and b1.height == 1, 'row map: block1 attached at [1, 2)')
  check(b2.start_row == 3 and b2.height == 1, 'row map: block2 attached at [3, 4)')
  local el, off = T.element_at_row_full(m, b, 3)
  check(el == block2 and off == 0, 'row 3 resolves to block2 (attached sub-entry)')
  el, off = T.element_at_row_full(m, b, 4)
  check(el == am and off == 4, 'row 4 resolves to (am, offset 4)')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('attached: o toggle collapses/expands the block in place and shifts the response rows', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  windowed_render(b, { AssistantMessageStart = {} }, false)
  local am = m.elements[1]
  windowed_render(b, { AssistantThinkingChunk = { content = 'T1\nT2' } }, false)
  local block = m.elements[2]
  windowed_render(b, { AssistantMessageChunk = { content = ' reply' } }, false)
  local l = lines_of(b)
  check(l[2] == '► [Thinking... press o to expand]' and l[3] == ' reply', 'collapsed hint above the reply')
  -- Expand in place: the block's full content opens between the label and the
  -- response; the response rows shift down.
  T.toggle_thinking(m, block, b, ns)
  l = lines_of(b)
  check(l[2] == '► [Thinking... press o to collapse]' and l[3] == 'T1' and l[4] == 'T2' and l[5] == ' reply',
    'expanded block content between the label and the shifted reply')
  check(row_of(m, am).height == 5 and row_of(m, block).start_row == 1 and row_of(m, block).height == 3,
    'row map: am [0, 5), block sub-entry [1, 4)')
  local el, off = T.element_at_row_full(m, b, 4)
  check(el == am and off == 4, 'shifted reply row resolves to (am, offset 4)')
  el, off = T.element_at_row_full(m, b, 2)
  check(el == block and off == 1, 'expanded block content row resolves to (block, offset 1)')
  -- Collapse again in place: the reply shifts back up.
  T.toggle_thinking(m, block, b, ns)
  l = lines_of(b)
  check(l[2] == '► [Thinking... press o to expand]' and l[3] == ' reply',
    'collapsed again; the reply shifted back to its row below the hint')
  check(row_of(m, am).height == 3 and row_of(m, block).start_row == 1 and row_of(m, block).height == 1,
    'row map consistent after the second toggle')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('attached: merge-reopen of a collapsed block renders the full content inside the am', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  windowed_render(b, { AssistantMessageStart = {} }, false)
  local am = m.elements[1]
  windowed_render(b, { AssistantThinkingChunk = { content = 'A1\nA2' } }, false)
  local block = m.elements[2]
  -- The settle flush collapses the open block (the pause path).
  T.render(m, T.close_open_elements(m), { buf = b, ns = ns, bulk = false })
  local l = lines_of(b)
  check(l[2] == '► [Thinking... press o to expand]', 'collapsed by the settle flush')
  -- A later run merges into the SAME attached block and reopens it in place;
  -- the full merged content renders INSIDE the am region.
  windowed_render(b, { AssistantThinkingChunk = { content = 'B1' } }, false)
  windowed_render(b, { AssistantThinkingChunk = { content = '\nB2' } }, false)
  l = lines_of(b)
  check(l[1] == '► ASSISTANT' and l[2] == 'A1' and l[3] == 'A2B1' and l[4] == 'B2',
    'merged reopen renders the FULL content inside the am region')
  check(T.content_of(block, 'content') == 'A1\nA2B1\nB2', 'model holds both runs in one attached block')
  local r = row_of(m, block)
  check(r.start_row == 1 and r.height == 3, 'row map: merged attached block at [1, 4)')
  check(row_of(m, am).start_row == 0 and row_of(m, am).height == 4, 'row map: am region [0, 4)')
  local el, off = T.element_at_row_full(m, b, 3)
  check(el == block and off == 2, 'merged content row resolves to (block, offset 2)')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('attached: bulk full projection folds the am as label + block lines + content', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  -- Seed the whole model without rendering, then render ONE bulk batch: the
  -- first bulk render is a one-time full projection of the model.
  local diffs = {}
  diffs[#diffs + 1] = T.apply(m, { AssistantMessageStart = {} })
  diffs[#diffs + 1] = T.apply(m, { AssistantThinkingChunk = { content = 'T1\nT2' } })
  local block = m.elements[2]
  diffs[#diffs + 1] = T.apply(m, { AssistantMessageChunk = { content = ' reply' } })
  local am = m.elements[1]
  T.render_batch(m, diffs, { buf = b, ns = ns, bulk = true })
  local l = lines_of(b)
  check(l[1] == '► ASSISTANT' and l[2] == '► [Thinking... press o to expand]' and l[3] == ' reply',
    'bulk projection folds [label, attached block lines, content]')
  check(row_of(m, am).start_row == 0 and row_of(m, am).height == 3, 'row map: folded am region [0, 3)')
  check(row_of(m, block).start_row == 1 and row_of(m, block).height == 1,
    'row map: attached block sub-entry at [1, 2) inside the folded am')
  local el, off = T.element_at_row_full(m, b, 1)
  check(el == block and off == 0, 'folded block row resolves to (block, offset 0)')
  el, off = T.element_at_row_full(m, b, 2)
  check(el == am and off == 2, 'folded content row resolves to (am, offset 2)')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('regression: the bulk full projection highlights headers without relying on a global ns', function()
  -- The full projection's folded-am highlight pass once referenced a bare
  -- global `ns` (nil in production, only masked by the test harness's _G.ns),
  -- which aborted the attach/initial-load render with "Invalid 'ns_id'" and
  -- left every header after the first folded am unhighlighted. Clear _G.ns so
  -- a regression of that class fails loudly here.
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  local diffs = {}
  diffs[#diffs + 1] = T.apply(m, { UserMessage = { content = 'hi' } })
  diffs[#diffs + 1] = T.apply(m, { AssistantMessageStart = {} })
  diffs[#diffs + 1] = T.apply(m, { AssistantThinkingChunk = { content = 'T1' } })
  local block = m.elements[3]
  diffs[#diffs + 1] = T.apply(m, { AssistantMessageChunk = { content = ' reply' } })
  local am = m.elements[2]
  local saved_ns = _G.ns
  _G.ns = nil
  local ok, err = pcall(function()
    T.with_modifiable(b, function()
      T.render_batch(m, diffs, { buf = b, ns = saved_ns, bulk = true })
    end)
  end)
  _G.ns = saved_ns
  check(ok, 'bulk projection renders without error when the global ns is absent: ' .. tostring(err))
  local l = lines_of(b)
  check(l[1] == '► USER' and l[2] == 'hi' and l[3] == '► ASSISTANT'
    and l[4] == '► [Thinking... press o to expand]' and l[5] == ' reply',
    'bulk projection renders all five rows')
  -- The folded am's label must carry its chrome highlight (TCodeAssistant
  -- span) applied through ctx.ns, not a nil global.
  local marks = vim.api.nvim_buf_get_extmarks(b, ns, 2, 3, { details = true })
  local found = false
  for _, mk in ipairs(marks) do
    local det = mk[4]
    if det and det.hl_group == 'TCodeAssistant' then found = true end
  end
  check(found, 'folded am label carries the TCodeAssistant chrome highlight')
  check(row_of(m, am).start_row == 2 and row_of(m, am).height == 3, 'row map: folded am region [2, 5)')
  check(row_of(m, block).start_row == 3 and row_of(m, block).height == 1, 'row map: attached block sub-entry at [3, 4)')
end)

-- ---------------------------------------------------- fix regressions

test('fix 1: a tool output chunk with a NUL byte renders as \\0 and keeps the row map', function()
  -- A NUL byte is an internal line break to nvim's buffer API; the renderer
  -- escapes it at the row boundary so a projected row never becomes multiple
  -- buffer rows. The model keeps the raw byte; the display shows '\0'.
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  windowed_render(b, { AssistantToolCallStart = { tool_call_id = 't1', tool_name = 'bash', tool_call_index = 0 } }, false)
  windowed_render(b, { ToolMessageStart = { tool_call_id = 't1', tool_args = '' } }, false)
  local tc = m.elements[1]
  windowed_render(b, { ToolOutputChunk = { tool_call_id = 't1', content = 'a\0b\nc' } }, false)
  check(T.content_of(tc, 'output') == 'a\0b\nc', 'model keeps the RAW NUL byte')
  local l = lines_of(b)
  check(l[1] == '► TOOL: [running] bash  [Ctrl-k to cancel]' and l[2] == '► Result' and l[3] == TC_FENCE,
    'tool label + Result header + open fence')
  check(l[4] == 'a\\0b' and l[5] == 'c', 'buffer rows show the escaped \\0 display text')
  -- Row-map consistency: the sum of element heights equals the line count.
  local st = T.get_renderer_state(m)
  local total = 0
  for _, el in ipairs(m.elements) do
    local entry = st.rows[el.id]
    if entry and entry.height then total = total + entry.height end
  end
  check(total == vim.api.nvim_buf_line_count(b), 'row map heights sum to the buffer line count')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)

test('fix 4: assistant content starting with ► joins its own row (structural split)', function()
  -- The streaming "no content rows yet" decision is STRUCTURAL (region height
  -- vs chrome rows + attached blocks), never a match on the text: content that
  -- merely STARTS with '► ' must stream/join exactly like any other content.
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  windowed_render(b, { AssistantMessageStart = {} }, false)
  local am = m.elements[1]
  windowed_render(b, { AssistantMessageChunk = { content = '► alpha' } }, false)
  windowed_render(b, { AssistantMessageChunk = { content = '► delta' } }, false)
  local l = lines_of(b)
  check(l[1] == '► ASSISTANT' and l[2] == '► alpha► delta', 'both chunks joined ONE content row (no extra row)')
  check(#l == 2, 'exactly label + one content row')
  local r = row_of(m, am)
  check(r.start_row == 0 and r.height == 2, 'row map: am at [0, 2)')
  -- The highlight content-classification is structural too: a thinking row
  -- that starts with '► ' is CONTENT, so it carries TCodeThinking.
  local m2 = T.reset_model()
  local b2 = new_buf()
  seed(b2, { '' })
  windowed_render(b2, { AssistantMessageStart = {} }, false)
  windowed_render(b2, { AssistantThinkingChunk = { content = '► think' } }, false)
  local block = m2.elements[2]
  windowed_render(b2, { AssistantThinkingChunk = { content = '► more' } }, false)
  local l2 = lines_of(b2)
  check(l2[1] == '► ASSISTANT' and l2[2] == '► think► more', 'thinking chunks starting with ► join one row')
  check(row_of(m2, block).start_row == 1 and row_of(m2, block).height == 1,
    'row map: block sub-entry at [1, 2) inside the am')
  local hl = false
  local marks = vim.api.nvim_buf_get_extmarks(b2, ns, 0, -1, { details = true })
  for _, mm in ipairs(marks) do
    if mm[4] and mm[4].hl_group == 'TCodeThinking' and mm[2] == 1 then hl = true end
  end
  check(hl, 'the ►-prefixed content row is highlighted as CONTENT (TCodeThinking)')
  check(vim.bo[b].modifiable == false and vim.bo[b2].modifiable == false, 'buffers non-modifiable')
end)

test('fix 9: a zero-height first element does not consume first_event (no leading blank row)', function()
  -- A zero-height first projection (media without a root, an empty end_info)
  -- must leave the buffer's initial single empty line alone: first_event
  -- survives, so the first REAL element replaces it — no leading blank row.
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  windowed_render(b, { AssistantMessageEnd = {} }, false)
  local info = m.elements[1]
  check(info.type == 'end_info', 'empty end_info added as the first element')
  local r = row_of(m, info)
  check(r.start_row == nil and r.height == 0, 'zero-height entry, no start_row')
  local l = lines_of(b)
  check(#l == 1 and l[1] == '', 'buffer still holds only the initial empty line')
  -- The user message that follows must land at row 0.
  windowed_render(b, { UserMessage = { content = 'first' } }, false)
  l = lines_of(b)
  check(l[1] == '► USER' and l[2] == 'first', 'row 0 is ► USER, no leading blank row')
  local st = T.get_renderer_state(m)
  local total = 0
  for _, el in ipairs(m.elements) do
    local entry = st.rows[el.id]
    if entry and entry.height then total = total + entry.height end
  end
  check(total == vim.api.nvim_buf_line_count(b), 'row map heights sum to the buffer line count')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
end)
